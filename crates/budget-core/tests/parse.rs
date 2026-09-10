//! Ayrıştırıcı testleri: sentetik fikstür sözleşmedeki numaralı durumların
//! tamamını kapsar (iki kart, devir satırı, ödeme/iade/taksit/FX, dip özeti
//! kimliği, tutar ayrıştırıcı, Türkçe normalleştirme, birebir çift taksit).

use budget_core::parse::{
    merchant_norm, parse_tr_amount, parse_ziraat_html, Direction, ParseError, TxnKind,
};

const FIXTURE: &str = include_str!("fixtures/ziraat_min.html");

/// Banka baskısı düzeni: dip özeti 9 hücreli etiket satırı + 9 hücreli
/// değer/işleç satırı olarak gelir.
const FIXTURE9: &str = include_str!("fixtures/ziraat_footer9.html");

#[test]
fn parses_whole_statement()
{
    let parsed = parse_ziraat_html(FIXTURE.as_bytes()).expect("fikstür çözümlenir");
    let s = &parsed.statement;
    assert_eq!(s.period_end, "2026-09-05");
    assert_eq!(s.next_period_end.as_deref(), Some("2026-10-05"));
    assert_eq!(s.due_date.as_deref(), Some("2026-09-25"));
    assert_eq!(s.next_due_date.as_deref(), Some("2026-10-25"));
    assert_eq!(s.previous_balance_minor, 100_000);
    assert_eq!(s.spend_minor, 389_556);
    assert_eq!(s.fees_minor, 0);
    assert_eq!(s.payments_minor, 105_000);
    assert_eq!(s.period_debt_minor, 384_556);
    assert_eq!(s.period_debt_usd_minor, 0);
    assert_eq!(s.min_pay_minor, 50_000);
    assert_eq!(s.card_limit_minor, Some(500_000));
    assert_eq!(s.available_limit_minor, Some(461_544));
    // Görseller soyulmuş baytların onaltılık SHA-256'sı.
    assert_eq!(s.source_sha256.len(), 64);
    assert!(s
        .source_sha256
        .bytes()
        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)));

    assert_eq!(parsed.txns.len(), 10);
    for (i, t) in parsed.txns.iter().enumerate() {
        assert_eq!(t.row_index, i as i64, "row_index atlanan satırsız sıfır tabanlı");
    }

    let p = &parsed.txns[3];
    assert_eq!(p.kind, TxnKind::Payment, "şube-hesaptan ödeme kredisi");
    assert_eq!(p.direction, Direction::Credit);
    assert_eq!(p.amount_minor, 100_000);
    assert_eq!(parsed.txns[4].kind, TxnKind::Refund);
    assert_eq!(parsed.txns[4].amount_minor, 5_000);
    assert_eq!(parsed.txns[2].kind, TxnKind::Installment);
    assert_eq!(parsed.txns[2].extra, "3 TAKSIT LI ORNEK");
    // kkred satırı: renk yön söylemez; borç kalır, USD sütunu boştur ve FX
    // açıklamada taşınır.
    let fx = &parsed.txns[5];
    assert_eq!(fx.kind, TxnKind::Pos);
    assert_eq!(fx.direction, Direction::Debit);
    assert_eq!(fx.amount_minor, 1_200);
    assert_eq!(fx.usd_minor, None);
    assert!(fx.merchant_raw.contains("USD 12.00"));
}

#[test]
fn binds_txns_to_preceding_card()
{
    let parsed = parse_ziraat_html(FIXTURE.as_bytes()).expect("fikstür çözümlenir");
    for t in &parsed.txns[..8] {
        assert_eq!(t.card_last4, "0001");
    }
    for t in &parsed.txns[8..] {
        assert_eq!(t.card_last4, "0002");
    }
}

#[test]
fn keeps_both_identical_installment_rows()
{
    let parsed = parse_ziraat_html(FIXTURE.as_bytes()).expect("fikstür çözümlenir");
    let norm = merchant_norm("ORNEK TAKSIT MAGZA");
    let dups: Vec<_> = parsed
        .txns
        .iter()
        .filter(|t| t.merchant_norm == norm)
        .collect();
    assert_eq!(dups.len(), 2, "aynı dosyadaki iki özdeş taksit satırı saklanır");
    assert_ne!(dups[0].row_index, dups[1].row_index);
    for t in &dups {
        assert_eq!(t.kind, TxnKind::Installment);
        assert_eq!(t.amount_minor, 75_000);
        assert_eq!(t.direction, Direction::Debit);
    }
}

#[test]
fn reconcile_mismatch_refuses_with_numbers()
{
    // Dip kutusundaki Harcamalarınız bir kuruş kaydırılır: kimlik bozulur.
    let broken = FIXTURE.replace("3.895,56", "3.895,57");
    assert_ne!(broken, FIXTURE);
    match parse_ziraat_html(broken.as_bytes()) {
        Err(ParseError::Reconcile {
            previous_minor,
            sum_debit_minor,
            fees_minor,
            sum_credit_minor,
            period_debt_minor,
            ..
        }) => {
            assert_eq!(previous_minor, 100_000);
            assert_eq!(sum_debit_minor, 389_556);
            assert_eq!(fees_minor, 0);
            assert_eq!(sum_credit_minor, 105_000);
            assert_eq!(period_debt_minor, 384_556);
        }
        other => panic!("Reconcile beklenirdi, gelen: {other:?}"),
    }
}

#[test]
fn amount_parser_cases()
{
    assert_eq!(parse_tr_amount("1.234,56").unwrap(), (123_456, false));
    assert_eq!(parse_tr_amount("1.234,56+").unwrap(), (123_456, true));
    assert_eq!(parse_tr_amount("0,00").unwrap(), (0, false));
    assert_eq!(parse_tr_amount("18.619,06 TL").unwrap(), (1_861_906, false));
    assert!(parse_tr_amount("abc").is_err());
}

#[test]
fn norm_folds_turkish_and_strips_trailing_city()
{
    assert_eq!(
        merchant_norm("BIM V020 30 AGUSTOS KAYSERİ"),
        merchant_norm("BIM V020 30 AGUSTOS KAYSERI")
    );
    assert_eq!(
        merchant_norm("BIM V020 30 AGUSTOS KAYSERI"),
        "BIM V020 30 AGUSTOS"
    );
    // Tüm satır şehirse dokunulmaz.
    assert_eq!(merchant_norm("KAYSERI"), "KAYSERI");
    assert_eq!(merchant_norm("A  B\tC"), "A B C");
}

#[test]
fn refuses_non_ziraat_file()
{
    let other = b"<html><head><title>Baska Bank Ekstre</title></head><body></body></html>";
    assert!(matches!(
        parse_ziraat_html(other),
        Err(ParseError::NotZiraat)
    ));
}

#[test]
fn parses_nine_cell_split_footer()
{
    let parsed = parse_ziraat_html(FIXTURE9.as_bytes()).expect("9 hücreli dip çözümlenir");
    let s = &parsed.statement;
    assert_eq!(s.period_end, "2026-08-05");
    assert_eq!(s.due_date.as_deref(), Some("2026-08-25"));
    assert_eq!(s.previous_balance_minor, 250_000);
    assert_eq!(s.spend_minor, 893_050);
    assert_eq!(s.fees_minor, 0);
    assert_eq!(s.payments_minor, 123_000);
    assert_eq!(s.period_debt_minor, 1_020_050);
    assert_eq!(s.period_debt_usd_minor, 0);
    assert_eq!(s.min_pay_minor, 85_000);
    assert_eq!(s.card_limit_minor, Some(1_500_000));
    assert_eq!(s.available_limit_minor, Some(457_000));

    // İşlem toplamları dip kimliğine 0 kuruş hata ile oturur:
    // 250.000 + 893.050 + 0 − 123.000 = 1.020.050.
    let debit: i64 = parsed
        .txns
        .iter()
        .filter(|t| t.direction == Direction::Debit)
        .map(|t| t.amount_minor)
        .sum();
    let credit: i64 = parsed
        .txns
        .iter()
        .filter(|t| t.direction == Direction::Credit)
        .map(|t| t.amount_minor)
        .sum();
    assert_eq!(debit, 893_050);
    assert_eq!(credit, 123_000);
    assert_eq!(parsed.txns.len(), 3);
    assert!(parsed.txns.iter().all(|t| t.card_last4 == "0001"));
    let pay = &parsed.txns[2];
    assert_eq!(pay.kind, TxnKind::Payment);
    assert_eq!(pay.direction, Direction::Credit);
}

#[test]
fn footer_multi_boxes_in_single_row()
{
    // Beş kutu satırını tek 10 hücreli satıra birleştir: tüm kutular
    // çıkarılmalı, yalnızca ilk eşleşen değil.
    let merged = FIXTURE.replace("</tr>\n  <tr><td>", "<td>");
    assert_ne!(merged, FIXTURE);
    let parsed = parse_ziraat_html(merged.as_bytes()).expect("tek satır dip çözümlenir");
    let s = &parsed.statement;
    assert_eq!(s.previous_balance_minor, 100_000);
    assert_eq!(s.spend_minor, 389_556);
    assert_eq!(s.fees_minor, 0);
    assert_eq!(s.payments_minor, 105_000);
    assert_eq!(s.period_debt_minor, 384_556);
}
