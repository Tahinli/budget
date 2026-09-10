//! budget-web: topcoat sunucusu ve ekstre içe aktarım CLI'ı.
//!
//! Argümansız `budget-web` sunucudur; `budget-web import <ekstre.html>`
//! dosyayı çözüp depoya yazar ve raporu basar — çözüm ya da uzlaşma
//! hatasında 2 ile çıkar. Tarayıcısız canlı duman testi tam olarak budur.

mod config;
mod i18n;
mod layout;
mod pages;
mod server;

use std::sync::Arc;

use budget_core::parse::parse_ziraat_html;
use budget_core::store::TursoStore;
use topcoat::Result;
use topcoat::asset::{AssetBundle, RouterBuilderAssetExt};
use topcoat::cookie::RouterBuilderCookieExt;
use topcoat::router::{route, BodyLimit, Router, RouterBuilderDiscoverExt};
use topcoat::runtime::RouterBuilderRuntimeExt;

#[route(GET "/healthz")]
async fn healthz() -> Result<&'static str> {
    // Dağıtım bu yanıtı ittiği derlemeye karşı doğrular; portu tutan eski
    // bir süreç yeşil yanıtlamakla geçemez.
    Ok(concat!("ok ", env!("BUDGET_BUILD_SHA")))
}


const USAGE: &str = "\
budget-web — Ziraat Katılım harcama takibi

KULLANIM:
  budget-web                     sunucuyu config/budget.toml ile başlatır
  budget-web import <DOSYA>      ekstreyi çözüp depoya yazar, rapor basar
  budget-web help                bu yardım
";

use layout::tr_money;

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("--help" | "-h" | "help") => println!("{USAGE}"),
        Some("import") => import_cli(&args[2..]).await,
        Some(other) => {
            eprintln!("budget: bilinmeyen komut: {other}\n{USAGE}");
            std::process::exit(2);
        }
        None => serve().await,
    }
}

/// `budget-web import <path>`: ekstreyi depoya yazar. Çözüm/uzlaşma
/// hatasında çıkış 2; depo hatalarında 1.
async fn import_cli(args: &[String]) {
    let Some(path) = args.first() else {
        eprintln!("budget import: ekstre dosyası gerekli\n{USAGE}");
        std::process::exit(2);
    };
    let config = match config::Config::load() {
        Ok(config) => config,
        Err(problem) => {
            eprintln!("budget: {problem}");
            std::process::exit(2);
        }
    };
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(err) => {
            eprintln!("budget import: {path} okunamadı: {err}");
            std::process::exit(2);
        }
    };
    let parsed = match parse_ziraat_html(&bytes) {
        Ok(parsed) => parsed,
        Err(err) => {
            eprintln!("budget import: {err}");
            std::process::exit(2);
        }
    };

    let head = &parsed.statement;
    println!(
        "ekstre   dönem sonu {} · dönem borcu {} · asgari ödeme {}",
        head.period_end,
        tr_money(head.period_debt_minor),
        tr_money(head.min_pay_minor)
    );

    let store = match TursoStore::open(&config.database, &config.storage).await {
        Ok(store) => store,
        Err(err) => {
            eprintln!("budget import: depo açılamadı: {err}");
            std::process::exit(1);
        }
    };
    match store.import(parsed).await {
        Ok(report) if report.duplicate => {
            println!(
                "kopya    bu ekstre zaten yüklü ({} işlem), yeni kayıt yazılmadı",
                report.txn_count
            );
        }
        Ok(report) => {
            println!(
                "aktarıldı  {} işlem · etiketli {} · etiketsiz {}",
                report.txn_count, report.labeled, report.unlabeled
            );
        }
        Err(err) => {
            eprintln!("budget import: {err}");
            std::process::exit(1);
        }
    }
}

async fn serve() {
    let config = match config::Config::load() {
        Ok(config) => config,
        Err(problem) => {
            eprintln!("budget: {problem}");
            std::process::exit(2);
        }
    };
    for line in config.report() {
        println!("budget    {line}");
    }

    // Paket doğrulaması ikilinin nesline bağlar: parmak izi tutmayan paket
    // açılışı reddettirir, karışık nesil hiç başlamaz.
    let bundle = AssetBundle::load().unwrap_or_else(|err| {
        eprintln!("budget: asset paketi yüklenemedi: {err}");
        eprintln!("budget: `topcoat asset bundle -p budget-web` çalıştırıp yeniden deneyin");
        std::process::exit(2);
    });
    let stylesheet = match server::stylesheet_guard(&bundle) {
        Ok(line) => line,
        Err(problem) => {
            eprintln!("budget: {problem}");
            std::process::exit(2);
        }
    };
    println!("budget    {stylesheet}");

    // Tek süreç tek veritabanı: turso tek-yazıcı motordur.
    let store = Arc::new(
        TursoStore::open(&config.database, &config.storage)
            .await
            .expect("veritabanı açılamadı"),
    );

    let router = Router::builder()
        .discover()
        .runtime()
        // Ekstre yüklemeleri 8 MB'ı aşabilir; sınır /import'ta gevşetilir.
        .layer(BodyLimit::max(8 * 1024 * 1024).at("/import"))
        .cookies()
        .assets(bundle)
        .app_context(store)
        .build();

    // `topcoat::start` HOST/PORT'u ortamdan okur; dinleme adresi
    // config/budget.toml kararıdır, açıkça bağlanır.
    let listener = tokio::net::TcpListener::bind(&config.listen)
        .await
        .expect("dinleme adresi bağlanamadı");
    println!("budget    dinliyor http://{}", config.listen);
    topcoat::serve_until(listener, router, shutdown_signal())
        .await
        .expect("sunucu hatası");
}

/// Sürecin durdurulma çözümü: Ctrl+C ya da servis yöneticisinden SIGTERM.
async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("ctrl_c dinlenemedi");
    };
    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("SIGTERM dinlenemedi")
            .recv()
            .await;
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
}
