//! `config/budget.toml` — CWD'den okunur; dosya yoksa geliştirme
//! varsayılanları o yola yazılır ve açılış sürer (doldurulacak OIDC yok).
//! HOST/PORT ortam değişkenleri asla okunmaz: dinleme adresi yalnızca bu
//! dosyanın kararıdır.

use std::path::{Path, PathBuf};

use serde::Deserialize;

const PATH: &str = "config/budget.toml";

/// Yazılan varsayılan dosyanın içeriği; `config/budget.toml.example` ile aynı.
pub const DEFAULTS: &str = "\
# budget yapılandırması. `budget-web` ilk açılışta bu içeriği CWD altındaki
# `config/budget.toml`'a yazar; değiştirmek için dosyayı düzenin.
# Veritabanı ve storage yolları CWD'ye göredir.
database = \"budget.db\"
storage = \"storage\"
listen = \"127.0.0.1:7656\"
";

#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    #[serde(default = "default_database")]
    pub database: PathBuf,
    #[serde(default = "default_storage")]
    pub storage: PathBuf,
    #[serde(default = "default_listen")]
    pub listen: String,
}

fn default_database() -> PathBuf {
    PathBuf::from("budget.db")
}

fn default_storage() -> PathBuf {
    PathBuf::from("storage")
}

fn default_listen() -> String {
    "127.0.0.1:7656".to_string()
}

impl Config {
    /// Yapılandırmayı oku; dosya yoksa varsayılanları yaz ve varsayılanlarla
    /// dön. Var olan ama bozuk dosya hatayla durur — iki süreç iki farklı
    /// dosya sanıp yazmasın.
    pub fn load() -> Result<Config, String> {
        let path = Path::new(PATH);
        if !path.exists() {
            let parent = path.parent().unwrap_or(Path::new("."));
            std::fs::create_dir_all(parent).map_err(|e| format!("{PATH} kurulamadı: {e}"))?;
            std::fs::write(path, DEFAULTS).map_err(|e| format!("{PATH} yazılamadı: {e}"))?;
            println!("budget    {PATH} yoktu, varsayılanlar yazıldı");
            return toml::from_str(DEFAULTS).map_err(|e| format!("{PATH}: {e}"));
        }
        let raw = std::fs::read_to_string(path).map_err(|e| format!("{PATH} okunamadı: {e}"))?;
        toml::from_str(&raw).map_err(|e| format!("{PATH}: {e}"))
    }

    /// Önyükleme günlüğü satırları: hangi dosyada olduğumuz tek yerde söylensin.
    pub fn report(&self) -> Vec<String> {
        vec![
            format!("config   {PATH}"),
            format!("database {}", self.database.display()),
            format!("storage  {}", self.storage.display()),
            format!("listen   {}", self.listen),
        ]
    }
}
