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
    let expected = "#/components/schemas/LoginResponse";
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
