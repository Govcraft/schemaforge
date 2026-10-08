#![cfg(feature = "server")]
//! Export the public redirect contract without depending on a running forge.
#[test]
fn export_reuses_login_response_and_only_lists_compiled_oauth_routes() {
    let directory = tempfile::tempdir().unwrap();
    let schema = directory.path().join("note.schema");
    let output = directory.path().join("openapi.json");
    std::fs::write(&schema, "schema Note { title: text required }").unwrap();
    assert_cmd::cargo::cargo_bin_cmd!("schemaforge")
        .args([
            "export",
            "openapi",
            "--base-path",
            "/api/v1/forge",
            "--output",
        ])
        .arg(&output)
        .arg(&schema)
        .assert()
        .success();
    let document: serde_json::Value =
        serde_json::from_slice(&std::fs::read(output).unwrap()).unwrap();
    // Both GET routes expose the same validated projection options.
    for path in [
        "/api/v1/forge/schemas/note/entities",
        "/api/v1/forge/schemas/note/entities/{id}",
    ] {
        let read = &document["paths"][path]["get"];
        let parameters = read["parameters"].as_array().unwrap();
        let fields = parameters
            .iter()
            .find(|parameter| parameter["name"] == "fields")
            .unwrap();
        assert_eq!(fields["in"], "query");
        assert_eq!(fields["required"], false);
        assert_eq!(fields["schema"]["type"], "string");
        assert_eq!(fields["schema"]["minLength"], 1);
        assert!(fields["description"]
            .as_str()
            .unwrap()
            .contains("authorization"));
        let resolve = parameters
            .iter()
            .find(|parameter| parameter["name"] == "resolve")
            .unwrap();
        assert_eq!(resolve["in"], "query");
        assert_eq!(resolve["schema"]["default"], "true");
        assert!(read["responses"]["400"].is_object());
    }
    let expected = "#/components/schemas/LoginResponse";
    let revocation = &document["paths"]["/api/v1/forge/auth/revocations"]["post"];
    assert_eq!(
        revocation["requestBody"]["content"]["application/json"]["schema"]["required"],
        serde_json::json!(["subject", "not_before"])
    );
    assert!(revocation["responses"]["403"].is_object());
    assert_eq!(
        document["paths"]["/api/v1/forge/auth/login"]["post"]["responses"]["200"]["content"]
            ["application/json"]["schema"]["$ref"],
        expected
    );
    assert!(document["components"]["schemas"]["LoginResponse"]["properties"]["token"].is_object());
    #[cfg(feature = "oauth")]
    {
        for route in [
            "providers",
            "{provider}/start",
            "{provider}/callback",
            "exchange",
        ] {
            assert!(document["paths"][format!("/api/v1/forge/auth/oauth/{route}")].is_object());
        }
        assert_eq!(
            document["paths"]["/api/v1/forge/auth/oauth/exchange"]["post"]["responses"]["200"]
                ["content"]["application/json"]["schema"]["$ref"],
            expected
        );
    }
    #[cfg(not(feature = "oauth"))]
    assert!(document["paths"]
        .as_object()
        .unwrap()
        .keys()
        .all(|path| !path.contains("/auth/oauth/")));
}

#[test]
fn export_lists_event_stream_only_when_compiled() {
    let directory = tempfile::tempdir().unwrap();
    let schema = directory.path().join("note.schema");
    let output = directory.path().join("openapi.json");
    std::fs::write(&schema, "schema Note { title: text required }").unwrap();
    assert_cmd::cargo::cargo_bin_cmd!("schemaforge")
        .args([
            "export",
            "openapi",
            "--base-path",
            "/api/v1/forge",
            "--output",
        ])
        .arg(&output)
        .arg(&schema)
        .assert()
        .success();
    let document: serde_json::Value =
        serde_json::from_slice(&std::fs::read(output).unwrap()).unwrap();
    let route = &document["paths"]["/api/v1/forge/schemas/{schema}/events"];
    #[cfg(feature = "sse")]
    {
        assert_eq!(
            route["get"]["responses"]["200"]["content"]["text/event-stream"]["schema"]["type"],
            "string"
        );
        assert_eq!(
            route["get"]["security"][0]["bearerAuth"],
            serde_json::json!([])
        );
    }
    #[cfg(not(feature = "sse"))]
    assert!(route.is_null());
}

fn invitation_document(spec_version: &str) -> serde_json::Value {
    let directory = tempfile::tempdir().unwrap();
    let schema = directory.path().join("note.schema");
    let output = directory.path().join("openapi.json");
    std::fs::write(&schema, "schema Note { title: text required }").unwrap();
    assert_cmd::cargo::cargo_bin_cmd!("schemaforge")
        .args([
            "export",
            "openapi",
            "--base-path",
            "/api/v1/forge",
            "--spec-version",
            spec_version,
            "--output",
        ])
        .arg(&output)
        .arg(&schema)
        .assert()
        .success();
    serde_json::from_slice(&std::fs::read(output).unwrap()).unwrap()
}

#[test]
fn exported_invitation_management_documents_scope_permissions_and_pagination() {
    let document = invitation_document("3.1.0");
    let list = &document["paths"]["/api/v1/forge/auth/invites"]["get"];
    let revoke = &document["paths"]["/api/v1/forge/auth/invites/{id}"]["delete"];
    for operation in [list, revoke] {
        assert_eq!(
            operation["security"],
            serde_json::json!([{ "bearerAuth": [] }])
        );
        let header = operation["parameters"]
            .as_array()
            .unwrap()
            .iter()
            .find(|parameter| parameter["name"] == "X-Active-Tenant")
            .unwrap();
        assert_eq!(header["in"], "header");
        assert_eq!(header["required"], false);
        for code in ["400", "401", "403", "500", "502"] {
            assert!(operation["responses"][code].is_object());
        }
    }
    assert!(list["description"]
        .as_str()
        .unwrap()
        .contains("ListInvites"));
    assert!(revoke["description"]
        .as_str()
        .unwrap()
        .contains("RevokeInvite"));
    let parameters = list["parameters"].as_array().unwrap();
    for (name, minimum, maximum, default) in [("limit", 1, 100, 50), ("offset", 0, 1_000_000, 0)] {
        let parameter = parameters
            .iter()
            .find(|parameter| parameter["name"] == name)
            .unwrap();
        assert_eq!(parameter["in"], "query");
        assert_eq!(parameter["schema"]["minimum"], minimum);
        assert_eq!(parameter["schema"]["maximum"], maximum);
        assert_eq!(parameter["schema"]["default"], default);
    }
    assert_eq!(
        list["responses"]["200"]["content"]["application/json"]["schema"]["$ref"],
        "#/components/schemas/ListInvitesResponse"
    );
    let id = revoke["parameters"]
        .as_array()
        .unwrap()
        .iter()
        .find(|parameter| parameter["name"] == "id")
        .unwrap();
    assert_eq!(id["in"], "path");
    assert_eq!(id["required"], true);
    assert!(revoke["responses"]["204"].is_object());
    assert!(revoke["responses"]["204"]["content"].is_null());
    assert!(revoke["responses"]["404"].is_object());
    assert!(revoke["responses"]["409"].is_object());
    assert!(revoke["requestBody"].is_null());
}

#[test]
fn exported_invitation_schemas_contain_only_safe_fields_and_explicit_nullability() {
    for version in ["3.1.0", "3.0.3"] {
        let document = invitation_document(version);
        let components = &document["components"]["schemas"];
        assert!(components["ForgeInvitation"].is_null());
        let invitation = &components["PendingInviteResponse"];
        assert_eq!(invitation["additionalProperties"], false);
        let properties = invitation["properties"].as_object().unwrap();
        let fields: std::collections::BTreeSet<_> = properties.keys().map(String::as_str).collect();
        assert_eq!(
            fields,
            std::collections::BTreeSet::from([
                "id",
                "email",
                "role",
                "inviter",
                "created_at",
                "expires_at"
            ])
        );
        assert_eq!(invitation["required"].as_array().unwrap().len(), 6);
        for credential in ["token", "jti", "invite_id", "accept_url"] {
            assert!(!properties.contains_key(credential));
        }
        for nullable_field in ["role", "inviter", "created_at", "expires_at"] {
            let field = &invitation["properties"][nullable_field];
            if version == "3.0.3" {
                assert_eq!(field["type"], "string");
                assert_eq!(field["nullable"], true);
            } else {
                assert_eq!(field["type"], serde_json::json!(["string", "null"]));
                assert!(field["nullable"].is_null());
            }
        }
        assert_eq!(
            invitation["properties"]["created_at"]["format"],
            "date-time"
        );
        assert_eq!(
            invitation["properties"]["expires_at"]["format"],
            "date-time"
        );
        let page = &components["ListInvitesResponse"];
        assert_eq!(page["additionalProperties"], false);
        assert_eq!(
            page["required"],
            serde_json::json!(["invitations", "next_offset"])
        );
        assert_eq!(
            page["properties"]["invitations"]["items"]["$ref"],
            "#/components/schemas/PendingInviteResponse"
        );
        if version == "3.0.3" {
            assert_eq!(page["properties"]["next_offset"]["type"], "integer");
            assert_eq!(page["properties"]["next_offset"]["nullable"], true);
        } else {
            assert_eq!(
                page["properties"]["next_offset"]["type"],
                serde_json::json!(["integer", "null"])
            );
        }
    }
}
