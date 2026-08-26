pub fn wrapper() -> bool {
    tiny_a::always_ok()
}

#[cfg(test)]
mod tests {
    #[test]
    fn calls_always_ok_without_asserting() {
        let _ = tiny_a::always_ok();
    }

    #[test]
    fn hang_if_true() {
        if tiny_a::maybe_hang() {
            loop {}
        }
    }
}
