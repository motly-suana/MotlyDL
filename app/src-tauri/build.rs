// Builds the Firefox add-on next to the Chromium extension, then runs Tauri's
// build. Both use the same code (extension/); only the manifest differs:
// Firefox runs a background script rather than a service worker, identifies
// add-ons by ID, and ignores Chrome's "key".

use std::fs;
use std::path::Path;

const FIREFOX_ID: &str = "motlydl@motly-suana.github.io"; // also in src/browser.rs

fn main() {
    let src = Path::new("../../extension");
    let out = Path::new("gen/extension-firefox");
    println!("cargo:rerun-if-changed=../../extension");
    let _ = fs::remove_dir_all(out);
    copy_dir(src, out).expect("couldn't copy the extension for Firefox");

    let mut manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(src.join("manifest.json")).unwrap()).expect("extension/manifest.json isn't valid JSON");
    let m = manifest.as_object_mut().unwrap();
    m.remove("key");
    if let Some(perms) = m.get_mut("permissions").and_then(|v| v.as_array_mut()) {
        if !perms.iter().any(|p| p == "webRequestBlocking") {
            perms.push(serde_json::json!("webRequestBlocking"));
        }
    }
    m.insert("background".into(), serde_json::json!({ "scripts": ["capture.js", "background.js"] }));
    m.insert(
        "browser_specific_settings".into(),
        // Mozilla requires new add-ons to declare the data they collect. The
        // add-on only hands downloads (and their cookies) to MotlyDL on the
        // same computer; nothing is sent anywhere else. Needs Firefox 140+.
        serde_json::json!({ "gecko": {
            "id": FIREFOX_ID,
            "strict_min_version": "140.0",
            "data_collection_permissions": { "required": ["none"] },
        } }),
    );
    fs::write(out.join("manifest.json"), serde_json::to_vec_pretty(&manifest).unwrap()).unwrap();

    tauri_build::build()
}

fn copy_dir(from: &Path, to: &Path) -> std::io::Result<()> {
    fs::create_dir_all(to)?;
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if name_str.ends_with(".test.mjs") || name_str.ends_with(".test.js") {
            continue;
        }
        let target = to.join(&name);
        if entry.file_type()?.is_dir() {
            copy_dir(&entry.path(), &target)?;
        } else {
            fs::copy(entry.path(), target)?;
        }
    }
    Ok(())
}
