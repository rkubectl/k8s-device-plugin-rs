//! Optional conversions at the Kubernetes API boundary.

use k8s_openapi::apimachinery::pkg::api::resource as apiresource;

use super::Quantity;
use super::QuantityParseError;

impl From<Quantity> for apiresource::Quantity {
    fn from(quantity: Quantity) -> Self {
        Self(quantity.into_string())
    }
}

impl From<&Quantity> for apiresource::Quantity {
    fn from(quantity: &Quantity) -> Self {
        Self(quantity.to_string())
    }
}

impl TryFrom<apiresource::Quantity> for Quantity {
    type Error = QuantityParseError;

    fn try_from(quantity: apiresource::Quantity) -> Result<Self, Self::Error> {
        quantity.0.parse()
    }
}

impl TryFrom<&apiresource::Quantity> for Quantity {
    type Error = QuantityParseError;

    fn try_from(quantity: &apiresource::Quantity) -> Result<Self, Self::Error> {
        quantity.0.parse()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn owned_and_borrowed_conversions_preserve_fractional_values() {
        for (input, expected) in [
            ("0.5", "500m"),
            ("1.5Gi", "1536Mi"),
            ("9007199254740993", "9007199254740993"),
        ] {
            let wire = apiresource::Quantity(input.into());
            let borrowed = Quantity::try_from(&wire).unwrap();
            let owned = Quantity::try_from(wire).unwrap();
            assert_eq!(owned, borrowed);
            assert_eq!(apiresource::Quantity::from(&borrowed).0, expected);
            assert_eq!(apiresource::Quantity::from(owned).0, expected);
        }
    }

    #[test]
    fn conversions_validate_the_unchecked_api_string_wrapper() {
        for input in ["", "++1", "1e3Ki", "NaN"] {
            let wire = apiresource::Quantity(input.into());
            assert!(Quantity::try_from(&wire).is_err());
            assert!(Quantity::try_from(wire).is_err());
        }
    }
}
