//! Sunucu yardımcıları: depo, oturum ve `budget.key`.

use std::path::Path;
use std::sync::Arc;

use budget_core::store::TursoStore;
use im_client::User;
use rand::Rng;
use topcoat::context::{Cx, memoize};
use topcoat::router::error::see_other;
use topcoat::router::request::uri;
use topcoat::router::response::{IntoResponse, Response};

/// Router'a `app_context` ile bağlanan tek depo; her işleyici buradan
/// erişir. Yokluğu programlama hatasıdır — builder her açılışta koyar.
pub fn store(cx: &Cx) -> &Arc<TursoStore> {
    topcoat::context::try_app_context::<Arc<TursoStore>>(cx)
        .expect("router her istekte depoyu taşır")
}

/// Bu isteğin kişisi, tarayıcıda oturum varsa. im-client her çağrıda
/// im'e sorar; `#[memoize]` o sorguyu tek isteğe sıkıştırır — işleyici
/// ve kabuk aynı içgörüyü paylaşır, istek başına tek içgörü çıkar.
#[memoize(as_ref)]
pub async fn current_user(cx: &Cx) -> Option<User> {
    im_client::current_user(cx).await
}

/// Kalan `local` satırlarının devri süreç başına bir kez: kişili ilk
/// istekte işaretlenir — açılışta kimse yoktur, kişiler istek başına gelir.
static CLAIMED_LOCAL: tokio::sync::OnceCell<()> = tokio::sync::OnceCell::const_new();

/// İsteğin kişisi; oturum yoksa `/auth/login?next=` yanıtı döner ve
/// işleyici onu olduğu gibi döndürür. Kişinin ilk isteğiyse eski kişisel
/// veritabanından kalan `local` satırları bu kişiye devredilir.
pub async fn require_user(cx: &Cx) -> Result<User, Response> {
    let Some(user) = current_user(cx).await.cloned() else {
        return Err(login_redirect(cx));
    };
    let store = store(cx);
    CLAIMED_LOCAL
        .get_or_init(|| async {
            if let Err(err) = store.claim_local(&user.sub).await {
                eprintln!("budget    claim_local: {err}");
            }
        })
        .await;
    Ok(user)
}

/// Oturum yok: tarayıcıyı giriş yoluna, geldigi yere dönecek biçimde yolla.
fn login_redirect(cx: &Cx) -> Response {
    let here = uri(cx);
    let target = match here.query() {
        Some(query) => format!("{}?{query}", here.path()),
        None => here.path().to_string(),
    };
    // Bileşim elden geldiği için ayrıştırma patlamaz; yine de yanıt
    // kurulamazsa açılış değil istek düşer.
    see_other(format!("/auth/login?next={}", urlencoded(&target)))
        .into_response(cx)
        .expect("giriş yönlendirmesi yanıta çevrilir")
}

fn urlencoded(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for byte in raw.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

/// Veritabanının yanındaki `budget.key`: oturum çerezlerini mühürleyen 32
/// bayt. Depoda durmaz, tek dağıtımca paylaşılır; yoksa üretilip 0600 ile
/// yazılır, boyu tutmayan dosya açılışta hatadır.
pub(crate) fn load_or_create_key(path: &Path) -> std::io::Result<[u8; 32]> {
    const KEY_BYTES: usize = 32;
    match std::fs::read(path) {
        Ok(bytes) => bytes.try_into().map_err(|bytes: Vec<u8>| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!(
                    "{} {} bayt taşıyor; anahtar {KEY_BYTES} bayttır",
                    path.display(),
                    bytes.len()
                ),
            )
        }),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            let mut key = [0u8; KEY_BYTES];
            rand::rng().fill_bytes(&mut key);
            std::fs::write(path, key)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
            }
            Ok(key)
        }
        Err(err) => Err(err),
    }
}

use sha2::{Digest, Sha256};
use topcoat::asset::AssetBundle;

/// Paketteki stil sayfasının baytları, ikiliye derlenen parmak iziyle
/// karşılaştırılır.
///
/// Çalıştırılabilirin yanındaki paket bu sürecin servis edebileceği tek stil
/// kaynağıdır ve topcoat onu ikilinin nesline bağlamaz: başka bir
/// dağıtımın bıraktığı paket aynı sevimlilikle yüklenir, sayfalar da
/// baytları günler eski bir stil sayfasına atıfta bulunur — bir tarayıcının
/// üretimde yakaladığı karışık nesil. `build.rs` derlenen stilin SHA-256'sını
/// ikiliye mühürler; buradaki doğrulama uyuşmayan paketi hizmet başlatmadan
/// yakalar.
///
/// Başlangıç günlüğü satırını ya da açılışın reddedilme sebebini döner.
pub fn stylesheet_guard(bundle: &AssetBundle) -> Result<String, String> {
    let expected = env!("BUDGET_STYLE_FINGERPRINT");
    let stylesheet = bundle
        .catalog()
        .assets()
        .find(|asset| {
            let name = asset.name();
            name.starts_with("main-") && name.ends_with(".css")
        })
        .ok_or_else(|| {
            format!(
                "asset paketi {} stil sayfası taşımıyor",
                bundle.dir().display()
            )
        })?;
    let bytes = std::fs::read(bundle.dir().join(stylesheet.name())).map_err(|err| {
        format!(
            "paketeki stil {} okunamadı: {err}",
            stylesheet.name()
        )
    })?;
    let actual = format!("sha256:{}", hex(&Sha256::digest(&bytes)));
    if actual != expected {
        return Err(format!(
            "asset paketi {} başka bir derlemeye ait: stil {} {actual}, ikili {expected} ile \
             derlendi; `topcoat asset bundle -p budget-web` çalıştırıp yeniden başlatın",
            bundle.dir().display(),
            stylesheet.name()
        ));
    }
    Ok(format!("assets   stil {} ({actual})", stylesheet.name()))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
