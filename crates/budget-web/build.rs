//! `style/main.scss`'i bu krate'in manifest dizini altındaki
//! `assets/main.css`'e derler; `asset!("assets/main.css")` tam orayı bekler.
//! Crate'e göreli konum, `OUT_DIR`'e göre değil — yoksa her derleme
//! karmasıyla stilin id'si oynar ve paket kimliği sabit kalmaz.
//!
//! Ayrıca derlenen stilin SHA-256'sını ikiliye mühürler: sunucu açılışta
//! paketteki baytları bu parmak izine karşı doğrular (`server.rs`'in
//! `stylesheet_guard`'ı), yabancı paket açılışı reddettirir.

use sha2::Digest;

fn main() {
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    let scss = format!("{manifest_dir}/style/main.scss");
    println!("cargo:rerun-if-changed={scss}");

    let css = grass::from_path(&scss, &grass::Options::default())
        .unwrap_or_else(|err| panic!("failed to compile {scss}: {err}"));
    let out_dir = format!("{manifest_dir}/assets");
    std::fs::create_dir_all(&out_dir).expect("failed to create assets/");
    std::fs::write(format!("{out_dir}/main.css"), &css)
        .expect("failed to write assets/main.css");

    let fingerprint = sha2::Sha256::digest(css.as_bytes());
    let mut hex = String::with_capacity(fingerprint.len() * 2);
    for byte in fingerprint {
        hex.push_str(&format!("{byte:02x}"));
    }
    println!("cargo:rustc-env=BUDGET_STYLE_FINGERPRINT=sha256:{hex}");

    // `/healthz`'ın söylediği derleme kimliği. Yerelde "dev"; farkı anlamlı
    // kılan ortam değişkeni. rerun-if-env-changed yüklü: olmadan rust-cache
    // isabeti önceki derlemenin sha'sını taşır.
    let sha = std::env::var("BUDGET_BUILD_SHA").unwrap_or_else(|_| "dev".into());
    println!("cargo:rustc-env=BUDGET_BUILD_SHA={sha}");
    println!("cargo:rerun-if-env-changed=BUDGET_BUILD_SHA");
}
