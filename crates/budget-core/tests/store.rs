//! Depo testleri: içe aktarım sayıları, mükerrer SHA koruması, etiketleme
//! kalıcılığı (sonraki içe aktarım takma ada otomatik bağlanır), gösterge
//! panosu ve gelen kutusu grupları.

use std::path::Path;

use budget_core::parse::{merchant_norm, parse_ziraat_html, TxnKind};
use budget_core::store::{TursoStore, TxnFilter, CAT_IADE, CAT_MARKET, CAT_ODEME};
use ulid::Ulid;

const FIXTURE: &str = include_str!("fixtures/ziraat_min.html");

async fn open_store() -> TursoStore {
    let storage = std::env::temp_dir().join(format!("budget-store-test-{}", Ulid::generate()));
    TursoStore::open(Path::new(":memory:"), &storage)
        .await
        .expect("bellek içi depo açılır")
}

/// İkinci ekstre: aynı tüccarlar, farklı tutarlar — dip kimliği yine tutar,
/// SHA bambaşka olur.
fn fixture_v2() -> Vec<u8> {
    FIXTURE
        .replace("99,00", "105,00")
        .replace("3.895,56", "3.901,56")
        .replace("3.845,56", "3.851,56")
        .into_bytes()
}

#[tokio::test]
async fn imports_statement_with_counts_and_payment_autolabel()
{
    let store = open_store().await;
    let report = store
        .import(parse_ziraat_html(FIXTURE.as_bytes()).expect("fikstür çözümlenir"))
        .await
        .expect("içe aktarım");
    assert!(!report.duplicate);
    assert_eq!(report.txn_count, 10);
    assert_eq!(report.labeled, 1, "yalnızca kart ödemesi otomatik etiketli");
    assert_eq!(report.unlabeled, 9);

    let statements = store.statements().await.expect("ekstreler");
    assert_eq!(statements.len(), 1);
    assert_eq!(statements[0].period_debt_minor, 384_556);
    assert_eq!(statements[0].previous_balance_minor, 100_000);

    let txns = store
        .list_txns(TxnFilter::default())
        .await
        .expect("işlemler");
    assert_eq!(txns.len(), 10);
    let payment = txns
        .iter()
        .find(|t| t.kind == TxnKind::Payment)
        .expect("ödeme işlemi");
    assert_eq!(payment.payee_name.as_deref(), Some("Kart ödemesi"));
    assert_eq!(payment.category_id.as_deref(), Some(CAT_ODEME));
}

#[tokio::test]
async fn duplicate_sha_is_reported_and_not_reinserted()
{
    let store = open_store().await;
    let first = store
        .import(parse_ziraat_html(FIXTURE.as_bytes()).expect("fikstür çözümlenir"))
        .await
        .expect("ilk içe aktarım");
    assert!(!first.duplicate);

    let second = store
        .import(parse_ziraat_html(FIXTURE.as_bytes()).expect("fikstür çözümlenir"))
        .await
        .expect("ikinci içe aktarım");
    assert!(second.duplicate);
    assert_eq!(second.txn_count, 10, "mevcut işlem sayısı raporlanır");
    assert_eq!(second.statement_id, first.statement_id);

    assert_eq!(store.statements().await.unwrap().len(), 1);
    assert_eq!(store.list_txns(TxnFilter::default()).await.unwrap().len(), 10);
}

#[tokio::test]
async fn label_persists_and_relinks_later_imports()
{
    let store = open_store().await;
    store
        .import(parse_ziraat_html(FIXTURE.as_bytes()).expect("fikstür çözümlenir"))
        .await
        .expect("ilk içe aktarım");

    let norm = merchant_norm("KAHVE ORNEK ANKARA");
    let report = store
        .label(&norm, "Kahve Dükkanı", CAT_MARKET)
        .await
        .expect("etiketleme");
    assert!(report.alias_created);
    assert_eq!(report.txns_updated, 1);

    // İkinci, farklı SHA'lı ekstre: aynı tüccar takma ada kendiliğinden
    // bağlanır.
    let second = store
        .import(parse_ziraat_html(&fixture_v2()).expect("v2 çözümlenir"))
        .await
        .expect("ikinci içe aktarım");
    assert!(!second.duplicate);
    assert_eq!(second.labeled, 2, "ödeme takma adı + kahve takma adı");

    let rows = store
        .list_txns(TxnFilter {
            statement_id: Some(second.statement_id.clone()),
            q: Some("KAHVE".into()),
            ..TxnFilter::default()
        })
        .await
        .expect("filtreli liste");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].payee_id.as_deref(), Some(report.payee_id.as_str()));

    // Yeniden etiketleme: takma ad güncellenir, iki ekstrenin işlemleri de.
    let relabel = store
        .label(&norm, "Kahve Dükkanı", CAT_IADE)
        .await
        .expect("yeniden etiketleme");
    assert!(!relabel.alias_created);
    assert_eq!(relabel.txns_updated, 2);
}

#[tokio::test]
async fn dashboard_unlabeled_groups_and_seed_categories()
{
    let store = open_store().await;

    let cats = store.categories().await.expect("tohum kategoriler");
    assert_eq!(cats.len(), 14);
    assert_eq!(cats[0].id, CAT_MARKET, "sort 10 ilk sırada");
    assert!(cats.iter().any(|c| c.id == CAT_IADE));

    store
        .import(parse_ziraat_html(FIXTURE.as_bytes()).expect("fikstür çözümlenir"))
        .await
        .expect("içe aktarım");

    let groups = store.unlabeled_groups().await.expect("gruplar");
    let market_norm = merchant_norm("MARKET ORNEK V020 KAYSERİ");
    let market = groups
        .iter()
        .find(|g| g.merchant_norm == market_norm)
        .expect("pazar grubu");
    assert_eq!(market.count, 1);
    assert_eq!(market.debit_minor, 25_000);
    assert_eq!(market.credit_minor, 0);

    let dash = store.dashboard(None).await.expect("pano");
    assert!(dash.statement.is_some());
    assert_eq!(dash.unlabeled_count, 9);
    assert_eq!(dash.unlabeled_debit_minor, 389_556);
    assert!(dash.spend_by_category.is_empty());
    assert_eq!(dash.monthly.len(), 2, "2026-08 ve 2026-09");
    assert_eq!(dash.monthly[0], ("2026-08".to_string(), 359_556, 105_000));
    assert_eq!(dash.monthly[1], ("2026-09".to_string(), 30_000, 0));

    store
        .label(&market_norm, "Ornek Market", CAT_MARKET)
        .await
        .expect("pazar etiketi");
    let dash = store.dashboard(None).await.expect("pano");
    assert_eq!(dash.unlabeled_count, 8);
    assert_eq!(dash.spend_by_category.len(), 1);
    assert_eq!(dash.spend_by_category[0].0.id, CAT_MARKET);
    assert_eq!(dash.spend_by_category[0].1, 25_000);
    assert_eq!(dash.top_payees, vec![("Ornek Market".to_string(), 25_000)]);
}
