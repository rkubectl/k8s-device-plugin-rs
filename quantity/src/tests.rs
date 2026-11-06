use super::*;

use serde_json as json;

#[test]
fn upstream_kubernetes_parse_values_are_preserved() {
    // The v1.36.4 TestQuantityParse table, with both signs, evaluated by
    // k8s.io/apimachinery v0.36.4. Kubernetes may preserve a non-canonical
    // input spelling; numeric equality is the publication invariant.
    let corpus = include_str!("tests/apimachinery-v1.36.4.jsonl");
    assert_eq!(corpus.lines().count(), 282);
    for line in corpus.lines() {
        let row = json::from_str::<json::Value>(line).unwrap();
        let input = row["input"].as_str().unwrap();
        let parsed = input.parse::<Quantity>();
        assert_eq!(parsed.is_ok(), row["valid"].as_bool().unwrap(), "{input}");
        if let Ok(quantity) = parsed {
            let expected = row["wire"].as_str().unwrap().parse::<Quantity>().unwrap();
            assert_eq!(quantity, expected, "{input}");
            assert_eq!(quantity, quantity.to_string().parse().unwrap(), "{input}");
        }
    }
}

#[test]
fn quantities_are_exact_across_units_and_large_integers() {
    for (left, right) in [
        ("0.5", "500m"),
        ("1.5Gi", "1536Mi"),
        ("1e3", "1k"),
        ("0.000000001", "1n"),
        ("0.0000000001", "1n"),
        ("9Ei", "9223372036854775807"),
    ] {
        assert_eq!(
            left.parse::<Quantity>().unwrap(),
            right.parse::<Quantity>().unwrap()
        );
    }
    assert_ne!(
        "9007199254740993".parse::<Quantity>().unwrap(),
        "9007199254740992".parse::<Quantity>().unwrap()
    );
}

#[test]
fn wire_forms_round_trip_without_loss_or_float_conversion() {
    for (input, expected) in [
        ("0", "0"),
        ("0.5", "500m"),
        ("1.5Gi", "1536Mi"),
        ("1024Mi", "1Gi"),
        ("1E", "1E"),
        ("1Ei", "1Ei"),
        ("0.0000000001", "1n"),
        ("-0.0000000001", "-1n"),
        ("0.000000000001Ki", "2n"),
        ("0.000001Ki", "1024u"),
        ("100.035Ki", "102435840m"),
        ("9Ei", "9223372036854775807"),
        ("-9Ei", "-9223372036854775807"),
        ("1.3E+6", "1300e3"),
        ("9007199254740993", "9007199254740993"),
        ("1e1000", "10e999"),
        ("1e-2147483648", "1e-9"),
        ("1000E", "1e21"),
        ("-0", "0"),
    ] {
        let quantity = input.parse::<Quantity>().unwrap();
        let wire = quantity.to_string();
        assert_eq!(wire, expected, "input {input}");
        assert_eq!(quantity, wire.parse::<Quantity>().unwrap(), "input {input}");
    }
}

#[test]
fn invalid_input_does_not_become_zero_or_a_different_quantity() {
    for input in [
        "",
        "+",
        "-",
        ".",
        "Gi",
        "NaN",
        "Inf",
        "++1",
        "--1",
        "1e3Ki",
        "1.2.3",
        "1K",
        "1mi",
        "1e",
        "1e+",
        "1e3.5",
        "0.5 ",
        " 0.5",
        "0\0",
        "١",
        "1e2147483648",
    ] {
        assert!(input.parse::<Quantity>().is_err(), "input {input:?}");
    }
}

#[test]
fn integer_conversions_remain_exact_and_default_is_zero() {
    assert_eq!(Quantity::default(), Quantity::from(0));
    for value in [i64::MIN, -1000, 0, 1, 1000, i64::MAX] {
        assert_eq!(Quantity::from(value), value.to_string().parse().unwrap());
    }
    assert_eq!(
        Quantity::from(u128::MAX),
        u128::MAX.to_string().parse().unwrap()
    );
}
