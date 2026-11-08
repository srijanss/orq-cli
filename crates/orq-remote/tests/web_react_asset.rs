//! The built React bundle is compiled into the server's fixed set of page files.

use std::net::{IpAddr, Ipv4Addr};

#[test]
fn the_react_bundle_is_a_compiled_in_script_served_on_its_exact_path_only() {
    let loopback = IpAddr::V4(Ipv4Addr::LOCALHOST);
    let asset = orq_remote::page_asset(loopback, "GET", "/app.react.js").expect("the bundle is a page file");
    assert!(asset.content_type.starts_with("text/javascript"));
    let built = std::fs::read_to_string(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("web/app.react.js")).unwrap();
    assert_eq!(asset.body, built);
    assert!(orq_remote::page_asset(loopback, "GET", "/app.react.js/").is_none());
    assert!(orq_remote::page_asset(loopback, "GET", "/web/app.react.js").is_none());
    assert!(orq_remote::page_asset(loopback, "POST", "/app.react.js").is_none());
}
