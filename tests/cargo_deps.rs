use std::fs;
use toml::Table;

#[test]
fn key_dependencies_present_with_correct_versions() {
    let manifest = fs::read_to_string(env!("CARGO_MANIFEST_DIR").to_owned() + "/Cargo.toml")
        .expect("Cargo.toml readable");
    let table: Table = manifest.parse().expect("valid Cargo.toml");

    let deps = table
        .get("dependencies")
        .and_then(|t| t.as_table())
        .expect("dependencies table");

    let cases = [
        ("axum", "0.8"),
        ("tokio", "1"),
        ("hyper", "1.5"),
        ("reqwest", "0.12"),
        ("clap", "4"),
        ("serde", "1"),
        ("jsonwebtoken", "9"),
        ("uuid", "1"),
        ("chrono", "0.4"),
        ("thiserror", "2"),
        ("anyhow", "1"),
        ("aws-sigv4", "1.4"),
        ("url", "2"),
        ("tower-http", "0.6"),
        ("tracing-subscriber", "0.3"),
        ("sha2", "0.10"),
        ("simd-json", "0.14"),
        ("rsa", "0.9"),
        ("dotenvy", "0.15"),
        ("rand", "0.8"),
    ];

    for (name, expected_prefix) in cases {
        let dep = deps
            .get(name)
            .expect(&format!("{name} should be present in Cargo.toml"));
        let version_str = match dep {
            toml::Value::String(s) => s.clone(),
            toml::Value::Table(t) => t
                .get("version")
                .and_then(|v| v.as_str())
                .expect(&format!("{name} has no version field"))
                .to_string(),
            _ => panic!("unexpected toml type for {name}"),
        };
        assert!(
            version_str.starts_with(expected_prefix),
            "{name} version should start with {expected_prefix}, got {version_str}"
        );
    }
}

#[test]
fn http_dependencies_have_required_features() {
    let manifest = fs::read_to_string(env!("CARGO_MANIFEST_DIR").to_owned() + "/Cargo.toml")
        .expect("Cargo.toml readable");
    let table: Table = manifest.parse().expect("valid Cargo.toml");
    let deps = table
        .get("dependencies")
        .and_then(|t| t.as_table())
        .expect("dependencies table");

    let hyper = deps
        .get("hyper")
        .and_then(|t| t.as_table())
        .expect("hyper should be a table");
    let hyper_features = hyper
        .get("features")
        .and_then(|v| v.as_array())
        .expect("hyper features should be an array");
    let hyper_feature_strs: Vec<&str> = hyper_features.iter().filter_map(|v| v.as_str()).collect();
    assert!(
        hyper_feature_strs.contains(&"client"),
        "hyper should have client feature"
    );
    assert!(
        hyper_feature_strs.contains(&"http1"),
        "hyper should have http1 feature"
    );
    assert!(
        hyper_feature_strs.contains(&"http2"),
        "hyper should have http2 feature"
    );

    let tower_http = deps
        .get("tower-http")
        .and_then(|t| t.as_table())
        .expect("tower-http should be a table");
    let tower_http_features = tower_http
        .get("features")
        .and_then(|v| v.as_array())
        .expect("tower-http features should be an array");
    let tower_http_feature_strs: Vec<&str> = tower_http_features
        .iter()
        .filter_map(|v| v.as_str())
        .collect();
    assert!(
        tower_http_feature_strs.contains(&"cors"),
        "tower-http should have cors feature"
    );
    assert!(
        tower_http_feature_strs.contains(&"trace"),
        "tower-http should have trace feature"
    );
    assert!(
        tower_http_feature_strs.contains(&"fs"),
        "tower-http should have fs feature"
    );

    let reqwest = deps
        .get("reqwest")
        .and_then(|t| t.as_table())
        .expect("reqwest should be a table");
    let reqwest_features = reqwest
        .get("features")
        .and_then(|v| v.as_array())
        .expect("reqwest features should be an array");
    let reqwest_feature_strs: Vec<&str> =
        reqwest_features.iter().filter_map(|v| v.as_str()).collect();
    assert!(
        reqwest_feature_strs.contains(&"json"),
        "reqwest should have json feature"
    );
    assert!(
        reqwest_feature_strs.contains(&"cookies"),
        "reqwest should have cookies feature"
    );
    assert!(
        reqwest_feature_strs.contains(&"gzip"),
        "reqwest should have gzip feature"
    );
    assert!(
        reqwest_feature_strs.contains(&"brotli"),
        "reqwest should have brotli feature"
    );
    assert!(
        reqwest_feature_strs.contains(&"socks"),
        "reqwest should have socks feature"
    );
}

#[test]
fn clap_has_required_features() {
    let manifest = fs::read_to_string(env!("CARGO_MANIFEST_DIR").to_owned() + "/Cargo.toml")
        .expect("Cargo.toml readable");
    let table: Table = manifest.parse().expect("valid Cargo.toml");
    let deps = table
        .get("dependencies")
        .and_then(|t| t.as_table())
        .expect("dependencies table");

    let clap = deps
        .get("clap")
        .and_then(|t| t.as_table())
        .expect("clap should be a table");
    let clap_features = clap
        .get("features")
        .and_then(|v| v.as_array())
        .expect("clap features should be an array");
    let clap_feature_strs: Vec<&str> = clap_features.iter().filter_map(|v| v.as_str()).collect();
    assert!(
        clap_feature_strs.contains(&"derive"),
        "clap should have derive feature"
    );
    assert!(
        clap_feature_strs.contains(&"env"),
        "clap should have env feature"
    );
}

#[test]
fn serde_has_derive_feature() {
    let manifest = fs::read_to_string(env!("CARGO_MANIFEST_DIR").to_owned() + "/Cargo.toml")
        .expect("Cargo.toml readable");
    let table: Table = manifest.parse().expect("valid Cargo.toml");
    let deps = table
        .get("dependencies")
        .and_then(|t| t.as_table())
        .expect("dependencies table");

    let serde = deps
        .get("serde")
        .and_then(|t| t.as_table())
        .expect("serde should be a table");
    let serde_features = serde
        .get("features")
        .and_then(|v| v.as_array())
        .expect("serde features should be an array");
    let serde_feature_strs: Vec<&str> = serde_features.iter().filter_map(|v| v.as_str()).collect();
    assert!(
        serde_feature_strs.contains(&"derive"),
        "serde should have derive feature"
    );
}

#[test]
fn uuid_has_required_features() {
    let manifest = fs::read_to_string(env!("CARGO_MANIFEST_DIR").to_owned() + "/Cargo.toml")
        .expect("Cargo.toml readable");
    let table: Table = manifest.parse().expect("valid Cargo.toml");
    let deps = table
        .get("dependencies")
        .and_then(|t| t.as_table())
        .expect("dependencies table");

    let uuid = deps
        .get("uuid")
        .and_then(|t| t.as_table())
        .expect("uuid should be a table");
    let uuid_features = uuid
        .get("features")
        .and_then(|v| v.as_array())
        .expect("uuid features should be an array");
    let uuid_feature_strs: Vec<&str> = uuid_features.iter().filter_map(|v| v.as_str()).collect();
    assert!(
        uuid_feature_strs.contains(&"v4"),
        "uuid should have v4 feature"
    );
    assert!(
        uuid_feature_strs.contains(&"serde"),
        "uuid should have serde feature"
    );
}

#[test]
fn chrono_has_serde_feature() {
    let manifest = fs::read_to_string(env!("CARGO_MANIFEST_DIR").to_owned() + "/Cargo.toml")
        .expect("Cargo.toml readable");
    let table: Table = manifest.parse().expect("valid Cargo.toml");
    let deps = table
        .get("dependencies")
        .and_then(|t| t.as_table())
        .expect("dependencies table");

    let chrono = deps
        .get("chrono")
        .and_then(|t| t.as_table())
        .expect("chrono should be a table");
    let chrono_features = chrono
        .get("features")
        .and_then(|v| v.as_array())
        .expect("chrono features should be an array");
    let chrono_feature_strs: Vec<&str> =
        chrono_features.iter().filter_map(|v| v.as_str()).collect();
    assert!(
        chrono_feature_strs.contains(&"serde"),
        "chrono should have serde feature"
    );
}

#[test]
fn dev_dependencies_present() {
    let manifest = fs::read_to_string(env!("CARGO_MANIFEST_DIR").to_owned() + "/Cargo.toml")
        .expect("Cargo.toml readable");
    let table: Table = manifest.parse().expect("valid Cargo.toml");
    let dev_deps = table
        .get("dev-dependencies")
        .and_then(|t| t.as_table())
        .expect("dev-dependencies table");

    assert!(
        dev_deps.contains_key("tempfile"),
        "tempfile should be present in dev-dependencies"
    );
    assert!(
        dev_deps.contains_key("wiremock"),
        "wiremock should be present in dev-dependencies"
    );
}
