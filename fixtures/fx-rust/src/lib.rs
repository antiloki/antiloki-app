//! Small on purpose: this fixture exists to be discovered, not to be impressive.
pub fn total(items: &[(u32, u32)]) -> u32 { items.iter().map(|(price, qty)| price * qty).sum() }

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn totals_a_basket() { assert_eq!(total(&[(2, 3), (1, 1)]), 7); }
    #[test] fn an_empty_basket_is_zero() { assert_eq!(total(&[]), 0); }
}
