//! The PWA icons are checked in, rendered by `web-src/icons/render.mjs`, and `web-src/build.mjs`
//! builds an optional service worker with a version stamped from the shell files.

mod web_deps;

use std::path::{Path, PathBuf};

use sha2::Digest;

fn crate_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf()
}

const ICONS: [(&str, u32); 4] = [("icon-192.png", 192), ("icon-512.png", 512), ("icon-maskable-512.png", 512), ("apple-touch-icon.png", 180)];

const PNG_SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];

struct Header {
    width: u32,
    height: u32,
    bit_depth: u8,
    colour_type: u8,
    interlace: u8,
}

/// The IHDR chunk, which a PNG must put first, right after the signature.
fn header(bytes: &[u8]) -> Header {
    assert!(bytes.len() > 33, "too short for a PNG");
    assert_eq!(bytes[..8], PNG_SIGNATURE, "no PNG signature");
    assert_eq!(&bytes[12..16], b"IHDR", "IHDR is not the first chunk");
    let word = |at: usize| u32::from_be_bytes(bytes[at..at + 4].try_into().unwrap());
    Header { width: word(16), height: word(20), bit_depth: bytes[24], colour_type: bytes[25], interlace: bytes[28] }
}

fn icon(name: &str) -> Vec<u8> {
    let path = crate_dir().join("web/icons").join(name);
    std::fs::read(&path).unwrap_or_else(|error| panic!("{} is checked in: {error}", path.display()))
}

#[test]
fn each_icon_is_an_rgba_png_of_its_stated_size() {
    for (name, size) in ICONS {
        let header = header(&icon(name));
        assert_eq!((header.width, header.height), (size, size), "{name}");
        assert_eq!(header.colour_type, 6, "{name} is RGBA");
        assert_eq!(header.bit_depth, 8, "{name} is 8-bit");
        assert_eq!(header.interlace, 0, "{name} is not interlaced");
    }
}

/// A minimal zlib inflater (stored, fixed and dynamic Huffman blocks), enough to read the
/// pixels back without a decoding crate. The Adler-32 trailer is not checked.
mod inflate {
    struct Bits<'a> {
        data: &'a [u8],
        at: usize,
    }

    impl Bits<'_> {
        fn bit(&mut self) -> u32 {
            let byte = *self.data.get(self.at / 8).expect("deflate stream ended early");
            let bit = (byte >> (self.at % 8)) & 1;
            self.at += 1;
            bit as u32
        }

        fn bits(&mut self, count: u32) -> u32 {
            (0..count).fold(0, |value, shift| value | (self.bit() << shift))
        }
    }

    struct Huffman {
        counts: [u16; 16],
        symbols: Vec<u16>,
    }

    impl Huffman {
        fn new(lengths: &[u8]) -> Huffman {
            let mut counts = [0u16; 16];
            for &length in lengths {
                counts[length as usize] += 1;
            }
            counts[0] = 0;
            let mut offsets = [0u16; 16];
            for length in 1..15 {
                offsets[length + 1] = offsets[length] + counts[length];
            }
            let mut symbols = vec![0u16; lengths.len()];
            for (symbol, &length) in lengths.iter().enumerate() {
                if length != 0 {
                    symbols[offsets[length as usize] as usize] = symbol as u16;
                    offsets[length as usize] += 1;
                }
            }
            Huffman { counts, symbols }
        }

        fn decode(&self, bits: &mut Bits) -> u16 {
            let (mut code, mut first, mut index) = (0i32, 0i32, 0i32);
            for length in 1..16 {
                code |= bits.bit() as i32;
                let count = self.counts[length] as i32;
                if code < first + count {
                    return self.symbols[(index + code - first) as usize];
                }
                index += count;
                first = (first + count) << 1;
                code <<= 1;
            }
            panic!("bad Huffman code");
        }
    }

    const LENGTH_BASE: [u16; 29] = [3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131, 163, 195, 227, 258];
    const LENGTH_EXTRA: [u8; 29] = [0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0];
    const DISTANCE_BASE: [u16; 30] = [1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537, 2049, 3073, 4097, 6145, 8193, 12289, 16385, 24577];
    const DISTANCE_EXTRA: [u8; 30] = [0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13, 13];
    const CODE_LENGTH_ORDER: [usize; 19] = [16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15];

    fn codes(bits: &mut Bits, out: &mut Vec<u8>, literals: &Huffman, distances: &Huffman) {
        loop {
            let symbol = literals.decode(bits) as usize;
            if symbol < 256 {
                out.push(symbol as u8);
                continue;
            }
            if symbol == 256 {
                return;
            }
            let at = symbol - 257;
            let length = LENGTH_BASE[at] as usize + bits.bits(LENGTH_EXTRA[at] as u32) as usize;
            let at = distances.decode(bits) as usize;
            let distance = DISTANCE_BASE[at] as usize + bits.bits(DISTANCE_EXTRA[at] as u32) as usize;
            assert!(distance <= out.len(), "distance too far back");
            for _ in 0..length {
                out.push(out[out.len() - distance]);
            }
        }
    }

    pub fn zlib(data: &[u8]) -> Vec<u8> {
        assert_eq!(data[0] & 0x0f, 8, "not a deflate zlib stream");
        let mut bits = Bits { data: &data[2..], at: 0 };
        let mut out = Vec::new();
        loop {
            let last = bits.bit() == 1;
            match bits.bits(2) {
                0 => {
                    bits.at = bits.at.div_ceil(8) * 8;
                    let start = bits.at / 8;
                    let length = u16::from_le_bytes([bits.data[start], bits.data[start + 1]]) as usize;
                    out.extend_from_slice(&bits.data[start + 4..start + 4 + length]);
                    bits.at = (start + 4 + length) * 8;
                }
                1 => {
                    let mut lengths = [0u8; 288];
                    lengths[..144].fill(8);
                    lengths[144..256].fill(9);
                    lengths[256..280].fill(7);
                    lengths[280..].fill(8);
                    codes(&mut bits, &mut out, &Huffman::new(&lengths), &Huffman::new(&[5u8; 30]));
                }
                2 => {
                    let literal_count = bits.bits(5) as usize + 257;
                    let distance_count = bits.bits(5) as usize + 1;
                    let code_length_count = bits.bits(4) as usize + 4;
                    let mut code_lengths = [0u8; 19];
                    for &at in &CODE_LENGTH_ORDER[..code_length_count] {
                        code_lengths[at] = bits.bits(3) as u8;
                    }
                    let code_length_codes = Huffman::new(&code_lengths);
                    let mut lengths = Vec::with_capacity(literal_count + distance_count);
                    while lengths.len() < literal_count + distance_count {
                        match code_length_codes.decode(&mut bits) {
                            symbol @ 0..=15 => lengths.push(symbol as u8),
                            16 => {
                                let previous = *lengths.last().expect("repeat with no previous length");
                                let times = 3 + bits.bits(2);
                                lengths.extend((0..times).map(|_| previous));
                            }
                            17 => lengths.extend((0..3 + bits.bits(3)).map(|_| 0)),
                            _ => lengths.extend((0..11 + bits.bits(7)).map(|_| 0)),
                        }
                    }
                    let literals = Huffman::new(&lengths[..literal_count]);
                    let distances = Huffman::new(&lengths[literal_count..]);
                    codes(&mut bits, &mut out, &literals, &distances);
                }
                _ => panic!("reserved deflate block type"),
            }
            if last {
                return out;
            }
        }
    }
}

/// The top-left pixel as RGBA. Every PNG filter leaves the first pixel of the first row as is,
/// since there is no pixel to its left or above it.
fn corner_pixel(bytes: &[u8]) -> [u8; 4] {
    header(bytes);
    let mut data = Vec::new();
    let mut at = 8;
    while at + 8 <= bytes.len() {
        let length = u32::from_be_bytes(bytes[at..at + 4].try_into().unwrap()) as usize;
        let kind = &bytes[at + 4..at + 8];
        if kind == b"IDAT" {
            data.extend_from_slice(&bytes[at + 8..at + 8 + length]);
        }
        at += 12 + length;
    }
    let rows = inflate::zlib(&data);
    rows[1..5].try_into().unwrap()
}

#[test]
fn the_rounded_icons_have_clear_corners_and_the_full_bleed_ones_the_sidebar_colour() {
    for name in ["icon-192.png", "icon-512.png"] {
        assert_eq!(corner_pixel(&icon(name))[3], 0, "{name} has a transparent corner");
    }
    for name in ["icon-maskable-512.png", "apple-touch-icon.png"] {
        assert_eq!(corner_pixel(&icon(name)), [0x15, 0x1b, 0x23, 0xff], "{name} has an opaque #151b23 corner");
    }
}

#[test]
fn rendering_the_icons_again_gives_the_checked_in_files_byte_for_byte() {
    if !web_deps::ready() {
        return;
    }
    let out = web_deps::private_temp_dir("icons");
    let rendered = std::process::Command::new("node").arg(crate_dir().join("web-src/icons/render.mjs")).arg("--out-dir").arg(&out).output().unwrap();
    assert!(rendered.status.success(), "{}", String::from_utf8_lossy(&rendered.stderr));
    for (name, _) in ICONS {
        let fresh = std::fs::read(out.join(name)).unwrap_or_else(|error| panic!("render.mjs --out-dir writes {name}: {error}"));
        assert!(fresh == icon(name), "web/icons/{name} is stale: run `node crates/orq-remote/web-src/icons/render.mjs`");
    }
    let _ = std::fs::remove_dir_all(&out);
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let path = entry.unwrap().path();
        let target = to.join(path.file_name().unwrap());
        if path.is_dir() {
            copy_dir(&path, &target);
        } else {
            std::fs::copy(&path, &target).unwrap();
        }
    }
}

/// A copy of `web-src` (its build inputs, with the installed `node_modules` linked in) next to a
/// copy of `web`, so the build sees the same layout as in the crate and touches nothing there.
struct Workspace {
    root: PathBuf,
}

const SW_FIXTURE: &str = "globalThis.orqShellVersion = __ORQ_SHELL_VERSION__;\n";

impl Workspace {
    fn new(tag: &str, with_worker: bool) -> Workspace {
        let root = web_deps::private_temp_dir(tag);
        let source = crate_dir().join("web-src");
        let web_src = root.join("web-src");
        std::fs::create_dir(&web_src).unwrap();
        for name in ["build.mjs", "package.json", "package-lock.json"] {
            std::fs::copy(source.join(name), web_src.join(name)).unwrap();
        }
        copy_dir(&source.join("src"), &web_src.join("src"));
        std::os::unix::fs::symlink(source.join("node_modules"), web_src.join("node_modules")).unwrap();
        copy_dir(&crate_dir().join("web"), &root.join("web"));
        if with_worker {
            std::fs::write(web_src.join("src/sw.js"), SW_FIXTURE).unwrap();
        }
        std::fs::create_dir(root.join("out")).unwrap();
        Workspace { root }
    }

    /// Runs the copied build with the bundle and the worker sent to `out/`; returns the stamp.
    fn build(&self) -> String {
        let out = self.root.join("out");
        let built = std::process::Command::new("node")
            .arg(self.root.join("web-src/build.mjs"))
            .arg("--out")
            .arg(out.join("app.react.js"))
            .arg("--sw-out")
            .arg(out.join("sw.js"))
            .output()
            .unwrap();
        assert!(built.status.success(), "{}", String::from_utf8_lossy(&built.stderr));
        std::fs::read_to_string(out.join("app.react.js.stamp")).unwrap()
    }

    fn worker(&self) -> Option<Vec<u8>> {
        std::fs::read(self.root.join("out/sw.js")).ok()
    }
}

impl Drop for Workspace {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn version_in(worker: &[u8]) -> String {
    let text = String::from_utf8(worker.to_vec()).unwrap();
    let start = text.find("orqShellVersion=\"").unwrap_or_else(|| panic!("no version assignment in {text:?}")) + "orqShellVersion=\"".len();
    let version = &text[start..start + text[start..].find('"').unwrap()];
    assert!(version.len() == 16 && version.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()), "{version:?} is 16 lowercase hex digits");
    assert!(!text.contains("__ORQ_SHELL_VERSION__"), "the placeholder is replaced: {text:?}");
    version.to_string()
}

#[test]
fn the_service_worker_is_built_with_a_stamped_version_only_when_its_source_exists() {
    if !web_deps::ready() {
        return;
    }
    let with = Workspace::new("sw-with", true);
    let stamp = with.build();
    let worker = with.worker().expect("--sw-out gets the built worker");
    version_in(&worker);
    let lines: Vec<&str> = stamp.lines().collect();
    assert_eq!(lines.len(), 3, "{stamp:?}");
    assert!(lines[0].starts_with("sources ") && lines[1].starts_with("bundle "), "{stamp:?}");
    assert_eq!(lines[2], format!("sw {}", hex(&sha2::Sha256::digest(&worker))));
    assert!(!with.root.join("web/sw.js").exists(), "--sw-out redirects the worker away from web/");

    let without = Workspace::new("sw-without", false);
    let stamp = without.build();
    assert!(without.worker().is_none(), "no worker is built without src/sw.js");
    assert!(!without.root.join("web/sw.js").exists());
    assert_eq!(stamp.lines().count(), 2, "{stamp:?}");
}

#[test]
fn changing_one_byte_of_a_shell_file_changes_the_worker_version() {
    if !web_deps::ready() {
        return;
    }
    let workspace = Workspace::new("sw-version", true);
    workspace.build();
    let before = version_in(&workspace.worker().expect("a worker is built"));
    let css = workspace.root.join("web/app.css");
    let mut bytes = std::fs::read(&css).unwrap();
    bytes[0] ^= 1;
    std::fs::write(&css, bytes).unwrap();
    workspace.build();
    let after = version_in(&workspace.worker().unwrap());
    assert_ne!(before, after, "the version covers web/app.css");
}

#[test]
fn removing_the_worker_source_removes_the_worker_an_earlier_build_wrote() {
    if !web_deps::ready() {
        return;
    }
    let workspace = Workspace::new("sw-removed", true);
    workspace.build();
    assert!(workspace.worker().is_some(), "the first build writes the worker");
    std::fs::remove_file(workspace.root.join("web-src/src/sw.js")).unwrap();
    let stamp = workspace.build();
    assert!(workspace.worker().is_none(), "a build without src/sw.js leaves no stale worker behind");
    assert_eq!(stamp.lines().count(), 2, "{stamp:?}");
}
