use std::collections::HashMap;
use std::sync::Arc;

use async_graphql::dynamic::{
    self, Field, FieldFuture, FieldValue, InputValue, Object, Scalar, Schema, TypeRef,
};
use schema_forge_core::types::{Cardinality, FieldType, SchemaDefinition};

use super::input_types::{
    build_create_input, build_filter_input, build_sort_field_enum, build_sort_input,
    build_sort_order_enum, build_update_input,
};
use super::resolvers::{
    resolve_create_entity, resolve_delete_entity, resolve_get_entity, resolve_list_entities,
    resolve_relation_many, resolve_relation_one, resolve_update_entity, ConnectionData,
    EntityFields,
};
use super::type_mapping::{
    dynamic_value_to_gql_value, field_type_to_type_ref, DATETIME_SCALAR, INT64_SCALAR, JSON_SCALAR,
};

/// Build a dynamic GraphQL schema from the given schema definitions.
///
/// Skips system schemas. Registers query/mutation fields for each non-system schema.
pub fn build_graphql_schema(schemas: &[SchemaDefinition]) -> Result<Schema, String> {
    let non_system: Vec<&SchemaDefinition> = schemas.iter().filter(|s| !s.is_system()).collect();

    // Build a lookup map for relation resolvers
    let schema_map: HashMap<String, &SchemaDefinition> = schemas
        .iter()
        .map(|s| (s.name.as_str().to_string(), s))
        .collect();

    let mut query = Object::new("Query");
    let mut mutation = Object::new("Mutation");

    // Track field names for collision detection
    let mut query_field_names: Vec<(String, String)> = Vec::new(); // (field_name, schema_name)

    // Collect all types to register
    let mut types_to_register: Vec<dynamic::Type> = vec![
        // Custom scalars
        dynamic::Type::Scalar(Scalar::new(DATETIME_SCALAR).description("RFC 3339 datetime string")),
        dynamic::Type::Scalar(Scalar::new(JSON_SCALAR).description("Arbitrary JSON value")),
        dynamic::Type::Scalar(
            Scalar::new(INT64_SCALAR).description("64-bit integer serialized as string"),
        ),
        // Shared SortOrder enum
        dynamic::Type::Enum(build_sort_order_enum()),
    ];

    for schema_def in &non_system {
        let schema_name = schema_def.name.as_str().to_string();
        let type_name = schema_name.clone();
        let connection_type_name = format!("{schema_name}Connection");
        let filter_type_name = format!("{schema_name}Filter");
        let sort_input_name = format!("{schema_name}SortInput");
        let create_input_name = format!("Create{schema_name}Input");
        let update_input_name = format!("Update{schema_name}Input");

        // 1. Build output Object type
        let object_type = build_output_type(schema_def, &schema_map, &type_name)?;
        types_to_register.push(dynamic::Type::Object(object_type));

        // 2. Build connection type
        let conn_type = build_connection_type(&connection_type_name, &type_name);
        types_to_register.push(dynamic::Type::Object(conn_type));

        // 3. Build input types
        types_to_register.push(dynamic::Type::InputObject(build_create_input(schema_def)));
        types_to_register.push(dynamic::Type::InputObject(build_update_input(schema_def)));
        types_to_register.push(dynamic::Type::InputObject(build_filter_input(schema_def)));
        types_to_register.push(dynamic::Type::Enum(build_sort_field_enum(schema_def)));
        types_to_register.push(dynamic::Type::InputObject(build_sort_input(schema_def)));

        // 4. Build enum types for Enum fields
        for field_def in &schema_def.fields {
            if let FieldType::Enum(ref variants) = field_def.field_type {
                let enum_type_name = format!("{}_{}", schema_name, field_def.name.as_str());
                let mut gql_enum = async_graphql::dynamic::Enum::new(&enum_type_name);
                for variant in variants.iter() {
                    gql_enum = gql_enum.item(async_graphql::dynamic::EnumItem::new(variant));
                }
                types_to_register.push(dynamic::Type::Enum(gql_enum));
            }
        }

        // 5. Build composite nested object types
        for field_def in &schema_def.fields {
            if let FieldType::Composite(ref inner_fields) = field_def.field_type {
                let nested_name = format!("{}_{}", schema_name, field_def.name.as_str());
                let mut nested_obj = Object::new(&nested_name);
                for inner_field in inner_fields {
                    let inner_field_name = inner_field.name.as_str().to_string();
                    let inner_field_type = &inner_field.field_type;
                    let inner_tr = field_type_to_type_ref(
                        &nested_name,
                        &inner_field_name,
                        inner_field_type,
                        inner_field.is_required(),
                    );
                    let field_name_owned = inner_field_name.clone();
                    nested_obj =
                        nested_obj.field(Field::new(&inner_field_name, inner_tr, move |ctx| {
                            let field_name = field_name_owned.clone();
                            FieldFuture::new(async move {
                                let parent = ctx
                                    .parent_value
                                    .try_downcast_ref::<async_graphql::Value>()
                                    .ok();
                                if let Some(async_graphql::Value::Object(map)) = parent {
                                    if let Some(val) = map.get(field_name.as_str()) {
                                        return Ok(Some(FieldValue::value(val.clone())));
                                    }
                                }
                                Ok(None)
                            })
                        }));
                }
                types_to_register.push(dynamic::Type::Object(nested_obj));
            }
        }

        // 6. Build query fields
        let get_field_name = lcfirst(&schema_name);
        let list_field_name = pluralize(&get_field_name);

        query_field_names.push((get_field_name.clone(), schema_name.clone()));
        query_field_names.push((list_field_name.clone(), schema_name.clone()));

        // Get by ID
        {
            let sn = schema_name.clone();
            let sd = Arc::new((*schema_def).clone());
            let tn = type_name.clone();
            query = query.field(
                Field::new(&get_field_name, TypeRef::named(&type_name), move |ctx| {
                    let sn = sn.clone();
                    let sd = sd.clone();
                    let tn = tn.clone();
                    FieldFuture::new(async move { resolve_get_entity(&ctx, &sn, &sd, &tn).await })
                })
                .argument(InputValue::new("id", TypeRef::named_nn(TypeRef::ID))),
            );
        }

        // List/query
        {
            let sn = schema_name.clone();
            let sd = Arc::new((*schema_def).clone());
            let tn = type_name.clone();
            query = query.field(
                Field::new(
                    &list_field_name,
                    TypeRef::named_nn(&connection_type_name),
                    move |ctx| {
                        let sn = sn.clone();
                        let sd = sd.clone();
                        let tn = tn.clone();
                        FieldFuture::new(
                            async move { resolve_list_entities(&ctx, &sn, &sd, &tn).await },
                        )
                    },
                )
                .argument(InputValue::new("filter", TypeRef::named(&filter_type_name)))
                .argument(InputValue::new(
                    "sort",
                    TypeRef::named_list(&sort_input_name),
                ))
                .argument(InputValue::new("limit", TypeRef::named(TypeRef::INT)))
                .argument(InputValue::new("offset", TypeRef::named(TypeRef::INT))),
            );
        }

        // 7. Build mutation fields

        // create
        {
            let sn = schema_name.clone();
            let sd = Arc::new((*schema_def).clone());
            let tn = type_name.clone();
            let mutation_name = format!("create{schema_name}");
            mutation = mutation.field(
                Field::new(&mutation_name, TypeRef::named_nn(&type_name), move |ctx| {
                    let sn = sn.clone();
                    let sd = sd.clone();
                    let tn = tn.clone();
                    FieldFuture::new(
                        async move { resolve_create_entity(&ctx, &sn, &sd, &tn).await },
                    )
                })
                .argument(InputValue::new(
                    "input",
                    TypeRef::named_nn(&create_input_name),
                )),
            );
        }

        // update
        {
            let sn = schema_name.clone();
            let sd = Arc::new((*schema_def).clone());
            let tn = type_name.clone();
            let mutation_name = format!("update{schema_name}");
            mutation = mutation.field(
                Field::new(&mutation_name, TypeRef::named_nn(&type_name), move |ctx| {
                    let sn = sn.clone();
                    let sd = sd.clone();
                    let tn = tn.clone();
                    FieldFuture::new(
                        async move { resolve_update_entity(&ctx, &sn, &sd, &tn).await },
                    )
                })
                .argument(InputValue::new("id", TypeRef::named_nn(TypeRef::ID)))
                .argument(InputValue::new(
                    "input",
                    TypeRef::named_nn(&update_input_name),
                )),
            );
        }

        // delete
        {
            let sn = schema_name.clone();
            let sd = Arc::new((*schema_def).clone());
            let mutation_name = format!("delete{schema_name}");
            mutation = mutation.field(
                Field::new(
                    &mutation_name,
                    TypeRef::named_nn(TypeRef::BOOLEAN),
                    move |ctx| {
                        let sn = sn.clone();
                        let sd = sd.clone();
                        FieldFuture::new(async move {
                            let result = resolve_delete_entity(&ctx, &sn, &sd).await?;
                            Ok(Some(FieldValue::value(result)))
                        })
                    },
                )
                .argument(InputValue::new("id", TypeRef::named_nn(TypeRef::ID))),
            );
        }
    }

    // Check for field name collisions
    check_field_collisions(&query_field_names)?;

    // Ensure Query has at least one field (async-graphql requires it)
    if non_system.is_empty() {
        query = query.field(Field::new(
            "_empty",
            TypeRef::named(TypeRef::BOOLEAN),
            |_ctx| FieldFuture::new(async { Ok(None::<FieldValue>) }),
        ));
    }

    // Build schema — only include Mutation if we have schemas
    let mutation_name = if non_system.is_empty() {
        None
    } else {
        Some(mutation.type_name())
    };

    // Build schema with depth and complexity limits
    let mut builder = Schema::build(query.type_name(), mutation_name, None)
        .limit_depth(10)
        .limit_complexity(1000)
        .register(query);

    if !non_system.is_empty() {
        builder = builder.register(mutation);
    }

    for ty in types_to_register {
        builder = builder.register(ty);
    }

    builder
        .finish()
        .map_err(|e| format!("GraphQL schema build failed: {e}"))
}

/// Build the output Object type for a schema, with field resolvers.
fn build_output_type(
    schema_def: &SchemaDefinition,
    schema_map: &HashMap<String, &SchemaDefinition>,
    type_name: &str,
) -> Result<Object, String> {
    let schema_name = schema_def.name.as_str().to_string();
    let mut obj = Object::new(type_name);

    // Always include `id: ID!`
    obj = obj.field(Field::new("id", TypeRef::named_nn(TypeRef::ID), |ctx| {
        FieldFuture::new(async move {
            let ef = ctx.parent_value.try_downcast_ref::<EntityFields>()?;
            Ok(Some(FieldValue::value(async_graphql::Value::String(
                ef.id.as_str().to_string(),
            ))))
        })
    }));

    // Per-field resolvers
    for field_def in &schema_def.fields {
        let field_name = field_def.name.as_str().to_string();
        let field_type = &field_def.field_type;

        // Relations get special resolver treatment
        match field_type {
            FieldType::Relation {
                target,
                cardinality,
            } => {
                let target_name = target.as_str().to_string();
                let target_type = target_name.clone();
                // Relation type ref is always nullable
                let type_ref = field_type_to_type_ref(&schema_name, &field_name, field_type, false);

                let target_def = schema_map.get(&target_name).map(|s| Arc::new((*s).clone()));
                let card = *cardinality;
                let fn_clone = field_name.clone();

                obj = obj.field(Field::new(&field_name, type_ref, move |ctx| {
                    let fn_clone = fn_clone.clone();
                    let target_name = target_name.clone();
                    let target_type = target_type.clone();
                    let target_def = target_def.clone();
                    let card = card;
                    FieldFuture::new(async move {
                        let parent = ctx.parent_value.try_downcast_ref::<EntityFields>()?;
                        let Some(td) = target_def else {
                            return Ok(None);
                        };
                        match card {
                            Cardinality::One => {
                                resolve_relation_one(
                                    &ctx,
                                    parent,
                                    &fn_clone,
                                    &target_name,
                                    &td,
                                    &target_type,
                                )
                                .await
                            }
                            Cardinality::Many => {
                                resolve_relation_many(
                                    &ctx,
                                    parent,
                                    &fn_clone,
                                    &target_name,
                                    &td,
                                    &target_type,
                                )
                                .await
                            }
                            _ => Ok(None),
                        }
                    })
                }));
            }
            _ => {
                // Non-relation field: resolve from EntityFields
                let required = field_def.is_required();
                let type_ref =
                    field_type_to_type_ref(&schema_name, &field_name, field_type, required);
                let ft_clone = field_type.clone();
                let fn_clone = field_name.clone();

                obj = obj.field(Field::new(&field_name, type_ref, move |ctx| {
                    let fn_clone = fn_clone.clone();
                    let ft_clone = ft_clone.clone();
                    FieldFuture::new(async move {
                        let parent = ctx.parent_value.try_downcast_ref::<EntityFields>()?;
                        match parent.fields.get(&fn_clone) {
                            Some(dv) => {
                                let gql_val = dynamic_value_to_gql_value(dv, Some(&ft_clone));
                                Ok(Some(FieldValue::value(gql_val)))
                            }
                            None => Ok(None),
                        }
                    })
                }));
            }
        }
    }

    Ok(obj)
}

/// Build a `{Schema}Connection` type.
fn build_connection_type(connection_name: &str, item_type_name: &str) -> Object {
    let item_tn = item_type_name.to_string();

    Object::new(connection_name)
        .field(Field::new(
            "items",
            TypeRef::named_nn_list(item_type_name),
            move |ctx| {
                let item_tn = item_tn.clone();
                FieldFuture::new(async move {
                    let conn = ctx.parent_value.try_downcast_ref::<ConnectionData>()?;
                    let items: Vec<FieldValue> = conn
                        .items
                        .iter()
                        .map(|ef| {
                            FieldValue::owned_any(EntityFields {
                                id: ef.id.clone(),
                                schema: ef.schema.clone(),
                                fields: ef.fields.clone(),
                            })
                            .with_type(item_tn.clone())
                        })
                        .collect();
                    Ok(Some(FieldValue::list(items)))
                })
            },
        ))
        .field(Field::new(
            "count",
            TypeRef::named_nn(TypeRef::INT),
            |ctx| {
                FieldFuture::new(async move {
                    let conn = ctx.parent_value.try_downcast_ref::<ConnectionData>()?;
                    Ok(Some(FieldValue::value(async_graphql::Value::Number(
                        conn.count.into(),
                    ))))
                })
            },
        ))
        .field(Field::new(
            "totalCount",
            TypeRef::named(TypeRef::INT),
            |ctx| {
                FieldFuture::new(async move {
                    let conn = ctx.parent_value.try_downcast_ref::<ConnectionData>()?;
                    match conn.total_count {
                        Some(tc) => Ok(Some(FieldValue::value(async_graphql::Value::Number(
                            tc.into(),
                        )))),
                        None => Ok(None),
                    }
                })
            },
        ))
}

/// Lowercase the first character of a string.
fn lcfirst(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) => c.to_lowercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

/// Simple English pluralization.
fn pluralize(s: &str) -> String {
    if s.ends_with("sh")
        || s.ends_with("ch")
        || s.ends_with('s')
        || s.ends_with('x')
        || s.ends_with('z')
    {
        format!("{s}es")
    } else if s.ends_with('y')
        && s.len() > 1
        && !matches!(s.as_bytes()[s.len() - 2], b'a' | b'e' | b'i' | b'o' | b'u')
    {
        format!("{}ies", &s[..s.len() - 1])
    } else {
        format!("{s}s")
    }
}

/// Check for field name collisions among query fields.
fn check_field_collisions(names: &[(String, String)]) -> Result<(), String> {
    let mut seen: HashMap<&str, &str> = HashMap::new();
    for (field_name, schema_name) in names {
        if let Some(existing_schema) = seen.get(field_name.as_str()) {
            return Err(format!(
                "GraphQL field name collision: '{}' is generated by both '{}' and '{}'",
                field_name, existing_schema, schema_name
            ));
        }
        seen.insert(field_name, schema_name);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use schema_forge_core::types::{
        Annotation, FieldDefinition, FieldModifier, FieldName, IntegerConstraints, SchemaId,
        SchemaName, TextConstraints,
    };

    fn make_schema(name: &str, fields: Vec<FieldDefinition>) -> SchemaDefinition {
        SchemaDefinition::new(
            SchemaId::new(),
            SchemaName::new(name).unwrap(),
            fields,
            vec![],
        )
        .unwrap()
    }

    fn make_system_schema(name: &str) -> SchemaDefinition {
        SchemaDefinition::new(
            SchemaId::new(),
            SchemaName::new(name).unwrap(),
            vec![FieldDefinition::new(
                FieldName::new("email").unwrap(),
                FieldType::Text(TextConstraints::unconstrained()),
            )],
            vec![Annotation::System],
        )
        .unwrap()
    }

    fn text_field(name: &str) -> FieldDefinition {
        FieldDefinition::new(
            FieldName::new(name).unwrap(),
            FieldType::Text(TextConstraints::unconstrained()),
        )
    }

    fn required_text_field(name: &str) -> FieldDefinition {
        FieldDefinition::with_modifiers(
            FieldName::new(name).unwrap(),
            FieldType::Text(TextConstraints::unconstrained()),
            vec![FieldModifier::Required],
        )
    }

    #[test]
    fn lcfirst_basic() {
        assert_eq!(lcfirst("Contact"), "contact");
        assert_eq!(lcfirst("MySchema"), "mySchema");
        assert_eq!(lcfirst("a"), "a");
        assert_eq!(lcfirst(""), "");
    }

    #[test]
    fn pluralize_regular() {
        assert_eq!(pluralize("contact"), "contacts");
        assert_eq!(pluralize("user"), "users");
    }

    #[test]
    fn pluralize_s_ending() {
        assert_eq!(pluralize("status"), "statuses");
        assert_eq!(pluralize("address"), "addresses");
    }

    #[test]
    fn pluralize_x_ending() {
        assert_eq!(pluralize("box"), "boxes");
    }

    #[test]
    fn pluralize_sh_ending() {
        assert_eq!(pluralize("dish"), "dishes");
    }

    #[test]
    fn pluralize_ch_ending() {
        assert_eq!(pluralize("match"), "matches");
    }

    #[test]
    fn pluralize_consonant_y() {
        assert_eq!(pluralize("company"), "companies");
        assert_eq!(pluralize("category"), "categories");
    }

    #[test]
    fn pluralize_vowel_y() {
        assert_eq!(pluralize("day"), "days");
        assert_eq!(pluralize("key"), "keys");
    }

    #[test]
    fn build_schema_single() {
        let schemas = vec![make_schema(
            "Contact",
            vec![required_text_field("name"), text_field("email")],
        )];
        let result = build_graphql_schema(&schemas);
        assert!(result.is_ok(), "schema build failed: {:?}", result.err());
    }

    #[test]
    fn build_schema_multiple() {
        let schemas = vec![
            make_schema("Contact", vec![required_text_field("name")]),
            make_schema("Company", vec![required_text_field("name")]),
        ];
        let result = build_graphql_schema(&schemas);
        assert!(result.is_ok(), "schema build failed: {:?}", result.err());
    }

    #[test]
    fn build_schema_skips_system() {
        let schemas = vec![
            make_schema("Contact", vec![required_text_field("name")]),
            make_system_schema("User"),
        ];
        let result = build_graphql_schema(&schemas);
        assert!(result.is_ok());
        // The schema should build without creating User type
    }

    #[test]
    fn build_schema_with_relation() {
        let schemas = vec![
            make_schema("Company", vec![required_text_field("name")]),
            SchemaDefinition::new(
                SchemaId::new(),
                SchemaName::new("Contact").unwrap(),
                vec![
                    required_text_field("name"),
                    FieldDefinition::new(
                        FieldName::new("company").unwrap(),
                        FieldType::Relation {
                            target: SchemaName::new("Company").unwrap(),
                            cardinality: Cardinality::One,
                        },
                    ),
                ],
                vec![],
            )
            .unwrap(),
        ];
        let result = build_graphql_schema(&schemas);
        assert!(result.is_ok(), "schema build failed: {:?}", result.err());
    }

    #[test]
    fn build_schema_with_various_types() {
        use schema_forge_core::types::{EnumVariants, FloatConstraints};
        let schemas = vec![SchemaDefinition::new(
            SchemaId::new(),
            SchemaName::new("Product").unwrap(),
            vec![
                required_text_field("name"),
                FieldDefinition::new(
                    FieldName::new("price").unwrap(),
                    FieldType::Float(FloatConstraints::unconstrained()),
                ),
                FieldDefinition::new(
                    FieldName::new("quantity").unwrap(),
                    FieldType::Integer(IntegerConstraints::with_range(0, 10000).unwrap()),
                ),
                FieldDefinition::new(FieldName::new("active").unwrap(), FieldType::Boolean),
                FieldDefinition::new(FieldName::new("created_at").unwrap(), FieldType::DateTime),
                FieldDefinition::new(FieldName::new("metadata").unwrap(), FieldType::Json),
                FieldDefinition::new(
                    FieldName::new("status").unwrap(),
                    FieldType::Enum(
                        EnumVariants::new(vec!["Active".into(), "Inactive".into(), "Draft".into()])
                            .unwrap(),
                    ),
                ),
            ],
            vec![],
        )
        .unwrap()];
        let result = build_graphql_schema(&schemas);
        assert!(result.is_ok(), "schema build failed: {:?}", result.err());
    }

    #[test]
    fn field_name_collision_detected() {
        // Two schemas that would produce the same query field name
        // This is contrived — in practice very unlikely
        let names = vec![
            ("contact".to_string(), "Contact".to_string()),
            ("contact".to_string(), "ContactDup".to_string()),
        ];
        let result = check_field_collisions(&names);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("collision"));
    }

    #[test]
    fn no_collision_with_different_names() {
        let names = vec![
            ("contact".to_string(), "Contact".to_string()),
            ("company".to_string(), "Company".to_string()),
        ];
        assert!(check_field_collisions(&names).is_ok());
    }

    #[test]
    fn build_empty_schema_list() {
        let result = build_graphql_schema(&[]);
        assert!(result.is_ok());
    }
}
