use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;

use orq_core::remote_inbox::RemoteConfig;
use orq_core::storage::StorageRoots;
use orq_remote::{Server, ServerState};

const TOKEN: &str = "web-assets-test-token-0123456789ab";

fn start(tag: &str) -> Server {
    let base = orq_test_support::process_temp_dir().join(format!("orq-remote-web-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let roots = StorageRoots { data: base.join("data"), config: base.join("config"), state: base.join("state"), cache: base.join("cache") };
    roots.create_all().unwrap();
    let state = Arc::new(ServerState { roots, config: RemoteConfig { token: TOKEN.to_string() }, project_id: "p-0a1b2c3d".into(), project_name: "orq-cli".into() });
    Server::start(TcpListener::bind("127.0.0.1:0").unwrap(), state).unwrap()
}

struct Reply {
    status: u16,
    head: String,
    body: String,
    bytes: Vec<u8>,
}

fn request(addr: SocketAddr, method: &str, path: &str, token: Option<&str>) -> Reply {
    let mut stream = TcpStream::connect(addr).unwrap();
    stream.set_read_timeout(Some(std::time::Duration::from_secs(10))).unwrap();
    let auth = token.map(|t| format!("Authorization: Bearer {t}\r\n")).unwrap_or_default();
    write!(stream, "{method} {path} HTTP/1.1\r\nHost: x\r\nConnection: close\r\n{auth}Content-Length: 0\r\n\r\n").unwrap();
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).expect("the response was cut short");
    let split = raw.windows(4).position(|w| w == b"\r\n\r\n").expect("no header/body separator");
    let head = String::from_utf8_lossy(&raw[..split]).to_string();
    let bytes = raw[split + 4..].to_vec();
    let body = String::from_utf8_lossy(&bytes).to_string();
    let status = head.split_whitespace().nth(1).and_then(|s| s.parse().ok()).unwrap_or(0);
    Reply { status, head, body, bytes }
}

fn header(reply: &Reply, name: &str) -> Option<String> {
    reply.head.lines().find_map(|line| {
        let (key, value) = line.split_once(':')?;
        key.eq_ignore_ascii_case(name).then(|| value.trim().to_string())
    })
}

const ASSETS: [(&str, &str); 8] = [
    ("/", "text/html"),
    ("/app.react.js", "text/javascript"),
    ("/app.css", "text/css"),
    ("/manifest.webmanifest", "application/manifest+json"),
    ("/icons/icon-192.png", "image/png"),
    ("/icons/icon-512.png", "image/png"),
    ("/icons/icon-maskable-512.png", "image/png"),
    ("/icons/apple-touch-icon.png", "image/png"),
];

#[test]
fn the_page_and_its_assets_are_served_without_a_token_with_safe_headers() {
    let server = start("serve");
    for (path, kind) in ASSETS {
        let reply = request(server.addr(), "GET", path, None);
        assert_eq!(reply.status, 200, "{path}: {}", reply.head);
        assert!(!reply.body.is_empty(), "{path}");
        assert!(header(&reply, "content-type").unwrap().starts_with(kind), "{path}: {}", reply.head);
        assert_eq!(header(&reply, "cache-control").as_deref(), Some("no-store"), "{path}");
        assert_eq!(header(&reply, "x-content-type-options").as_deref(), Some("nosniff"), "{path}");
        let csp = header(&reply, "content-security-policy").unwrap_or_else(|| panic!("{path}: no CSP"));
        for part in ["default-src 'none'", "script-src 'self'", "style-src 'self'", "connect-src 'self'", "frame-ancestors 'none'", "form-action 'none'", "base-uri 'none'"] {
            assert!(csp.contains(part), "{path}: {csp}");
        }
        assert!(!csp.contains("unsafe-inline") && !csp.contains("unsafe-eval") && !csp.contains('*'), "{path}: {csp}");

        let head = request(server.addr(), "HEAD", path, None);
        assert_eq!(head.status, 200, "{path}");
        assert!(head.body.is_empty(), "{path}");
        assert_eq!(header(&head, "content-type"), header(&reply, "content-type"), "{path}");
    }
}

#[test]
fn the_token_free_page_is_only_for_get_and_head_from_an_allowed_peer() {
    for peer in ["127.0.0.1", "::1", "100.64.0.1", "fd7a:115c:a1e0::1"] {
        for (path, _) in ASSETS {
            for method in ["GET", "HEAD"] {
                assert!(orq_remote::page_asset(peer.parse().unwrap(), method, path).is_some(), "{peer} {method} {path}");
            }
            assert!(orq_remote::page_asset(peer.parse().unwrap(), "POST", path).is_none(), "{peer} POST {path}");
        }
    }
    for peer in ["8.8.8.8", "192.168.1.5", "10.0.0.1", "100.63.255.255", "100.128.0.1", "2001:db8::1", "fd7a:115c:a1e1::1"] {
        for (path, _) in ASSETS {
            for method in ["GET", "HEAD"] {
                assert!(orq_remote::page_asset(peer.parse().unwrap(), method, path).is_none(), "{peer} {method} {path} must be refused");
            }
        }
    }
}

#[test]
fn the_page_pulls_in_its_script_and_style_by_url_and_carries_no_inline_code() {
    let server = start("page");
    let page = request(server.addr(), "GET", "/", None).body;
    assert!(page.contains("src=\"/app.react.js\"") && page.contains("href=\"/app.css\""), "{page}");
    let lower = page.to_lowercase();
    assert!(!lower.contains("<style") && !lower.contains(" style=") && !lower.contains(" onclick=") && !lower.contains("javascript:"), "{page}");
    assert!(!lower.contains("<form"), "{page}");
    assert_eq!(lower.matches("<script").count(), 1, "{page}");
    assert!(!lower.contains(TOKEN));
}

#[test]
fn only_get_and_head_are_served_and_other_methods_are_refused_like_everywhere_else() {
    let server = start("methods");
    let refusal = request(server.addr(), "POST", "/api/reviews", None);
    for (path, _) in ASSETS {
        for method in ["POST", "PUT", "DELETE", "PATCH"] {
            let reply = request(server.addr(), method, path, None);
            assert_eq!(reply.status, 401, "{method} {path}");
            assert_eq!(reply.body, refusal.body, "{method} {path}");
            // With the token the write is refused as read-only.
            assert_eq!(request(server.addr(), method, path, Some(TOKEN)).status, 405, "{method} {path}");
        }
    }
}

#[test]
fn data_paths_still_need_the_token_and_unknown_paths_get_the_usual_refusal() {
    let server = start("data");
    let refusal = request(server.addr(), "GET", "/api/reviews", None);
    assert_eq!(refusal.status, 401);
    for path in ["/nowhere", "/index.html", "/app.js", "/app.react.js/", "/app.js/extra", "/api", "//app.js", "/APP.JS"] {
        let reply = request(server.addr(), "GET", path, None);
        assert_eq!(reply.status, 401, "{path}");
        assert_eq!(reply.body, refusal.body, "{path}");
        assert_eq!(header(&reply, "content-type"), header(&refusal, "content-type"), "{path}");
    }
    assert_eq!(request(server.addr(), "GET", "/nowhere", Some(TOKEN)).status, 404);
}

#[test]
fn nothing_outside_the_compiled_in_set_can_be_read_through_the_page_path() {
    let server = start("escape");
    let manifest = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml")).unwrap();
    let marker = "[dev-dependencies]";
    assert!(manifest.contains(marker));
    for path in ["/../Cargo.toml", "/%2e%2e/Cargo.toml", "/app.js/../Cargo.toml", "/web/../Cargo.toml", "/web/index.html", "/src/lib.rs", "/Cargo.toml", "/%2e%2e%2fCargo.toml"] {
        for token in [None, Some(TOKEN)] {
            let reply = request(server.addr(), "GET", path, token);
            assert!(!reply.body.contains(marker) && !reply.body.contains("pub fn"), "{path}: {}", reply.body);
            assert!(reply.status == 401 || reply.status == 404 || reply.status == 400, "{path}: {}", reply.status);
        }
    }
}

const PNG_SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];

/// The width and height from a PNG's IHDR chunk, which must come first after the signature.
fn png_size(bytes: &[u8]) -> (u32, u32) {
    assert!(bytes.len() > 24, "too short for a PNG");
    assert_eq!(bytes[..8], PNG_SIGNATURE, "no PNG signature");
    assert_eq!(&bytes[12..16], b"IHDR", "IHDR is not the first chunk");
    let word = |at: usize| u32::from_be_bytes(bytes[at..at + 4].try_into().unwrap());
    (word(16), word(20))
}

#[test]
fn the_manifest_names_the_app_and_every_icon_it_lists_is_served_as_a_png_of_its_size() {
    let server = start("manifest");
    let reply = request(server.addr(), "GET", "/manifest.webmanifest", None);
    assert_eq!(reply.status, 200, "{}", reply.head);
    assert!(header(&reply, "content-type").unwrap().starts_with("application/manifest+json"), "{}", reply.head);
    let manifest: serde_json::Value = serde_json::from_str(&reply.body).expect("the manifest is JSON");
    for (field, value) in [("name", "Orq"), ("short_name", "Orq"), ("id", "/"), ("start_url", "/dashboard"), ("scope", "/"), ("display", "standalone")] {
        assert_eq!(manifest[field], value, "{field}: {manifest}");
    }
    // The splash screen matches the dark sidebar.
    let css = request(server.addr(), "GET", "/app.css", None).body;
    let dark = css.split(":root[data-theme=\"dark\"]").nth(1).expect("a dark theme block");
    let sidebar = dark.split("--sidebar:").nth(1).unwrap().split(';').next().unwrap().trim();
    assert_eq!(manifest["background_color"], sidebar, "{manifest}");
    assert_eq!(manifest["theme_color"], sidebar, "{manifest}");

    let icons = manifest["icons"].as_array().expect("icons");
    let listed: Vec<(String, String, String)> = icons.iter().map(|icon| (icon["src"].as_str().unwrap().to_string(), icon["sizes"].as_str().unwrap().to_string(), icon["purpose"].as_str().unwrap_or("any").to_string())).collect();
    for (sizes, purpose) in [("192x192", "any"), ("512x512", "any"), ("512x512", "maskable")] {
        assert!(listed.iter().any(|(_, s, p)| s == sizes && p == purpose), "{sizes} {purpose}: {listed:?}");
    }
    let mut paths: Vec<(String, u32)> = icons.iter().map(|icon| {
        assert_eq!(icon["type"], "image/png", "{icon}");
        let size: u32 = icon["sizes"].as_str().unwrap().split('x').next().unwrap().parse().unwrap();
        (icon["src"].as_str().unwrap().to_string(), size)
    }).collect();
    paths.push(("/icons/apple-touch-icon.png".to_string(), 180));
    for (path, size) in paths {
        let reply = request(server.addr(), "GET", &path, None);
        assert_eq!(reply.status, 200, "{path}: {}", reply.head);
        assert_eq!(header(&reply, "content-type").as_deref(), Some("image/png"), "{path}");
        assert_eq!(png_size(&reply.bytes), (size, size), "{path}");
        let on_disk = std::fs::read(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("web").join(path.trim_start_matches('/'))).unwrap();
        assert_eq!(reply.bytes, on_disk, "{path} is served byte for byte");
    }
}

#[test]
fn the_page_head_links_the_manifest_and_icons_and_the_csp_allows_only_those_from_self() {
    let server = start("head");
    let reply = request(server.addr(), "GET", "/", None);
    let head = reply.body.split("</head>").next().unwrap();
    for tag in [
        "<link rel=\"manifest\" href=\"/manifest.webmanifest\">",
        "<link rel=\"apple-touch-icon\" href=\"/icons/apple-touch-icon.png\">",
        "<link rel=\"icon\" type=\"image/png\" href=\"/icons/icon-192.png\">",
        "<meta name=\"apple-mobile-web-app-title\" content=\"Orq\">",
        "<title>Orq</title>",
    ] {
        assert!(head.contains(tag), "{tag} missing from {head}");
    }
    assert_eq!(
        header(&reply, "content-security-policy").as_deref(),
        Some("default-src 'none'; script-src 'self'; style-src 'self'; connect-src 'self'; manifest-src 'self'; img-src 'self'; frame-ancestors 'none'; form-action 'none'; base-uri 'none'")
    );
}

#[test]
fn installed_the_status_bar_takes_the_top_bar_colour_in_both_themes_and_the_page_runs_full_screen() {
    let server = start("chrome");
    let head = request(server.addr(), "GET", "/", None).body.split("</head>").next().unwrap().to_string();
    for tag in [
        // The top bar's --sidebar colour in each theme, so the status bar reads as part of it.
        "<meta name=\"theme-color\" media=\"(prefers-color-scheme: light)\" content=\"#0f172a\">",
        "<meta name=\"theme-color\" media=\"(prefers-color-scheme: dark)\" content=\"#151b23\">",
        "<meta name=\"apple-mobile-web-app-capable\" content=\"yes\">",
        "<meta name=\"apple-mobile-web-app-status-bar-style\" content=\"black-translucent\">",
    ] {
        assert!(head.contains(tag), "{tag} missing from {head}");
    }
    let css = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/web/app.css")).unwrap();
    assert!(css.contains("--sidebar: #0f172a;") && css.contains("--sidebar: #151b23;"), "the theme colours follow the top bar's");
}

#[test]
fn an_unknown_path_under_icons_is_not_found_and_not_the_page() {
    let server = start("icons-404");
    let page = request(server.addr(), "GET", "/", None).body;
    for path in ["/icons/", "/icons/nope.png", "/icons/icon-192.png/", "/icons/../icons/icon-192.png", "/icons", "/manifest.json"] {
        assert!(orq_remote::page_asset("127.0.0.1".parse().unwrap(), "GET", path).is_none(), "{path}");
        let reply = request(server.addr(), "GET", path, Some(TOKEN));
        assert!(reply.status == 404 || reply.status == 400, "{path}: {}", reply.status);
        assert_ne!(reply.body, page, "{path}");
    }
}
