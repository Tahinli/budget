//! Sunucu yardımcıları.

use std::sync::Arc;

use budget_core::store::TursoStore;
use topcoat::context::Cx;

/// Router'a `app_context` ile bağlanan tek depo; her işleyici buradan
/// erişir. Yokluğu programlama hatasıdır — builder her açılışta koyar.
pub fn store(cx: &Cx) -> &Arc<TursoStore> {
    topcoat::context::try_app_context::<Arc<TursoStore>>(cx)
        .expect("router her istekte depoyu taşır")
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
