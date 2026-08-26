pub fn add(a: i32, b: i32) -> i32 {
    a + b
}

pub fn uncovered() -> i32 {
    7
}

pub fn always_ok() -> bool {
    true
}

pub fn greeting() -> String {
    "hello".into()
}

pub fn maybe_hang() -> bool {
    false
}

pub fn flag() -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_only_helper() -> i32 {
        99
    }

    #[test]
    fn add_works() {
        assert_eq!(add(2, 3), 5);
        let _ = test_only_helper();
    }

    #[test]
    fn greeting_works() {
        assert_eq!(greeting(), "hello");
    }

    #[test]
    fn flag_is_true() {
        assert!(flag());
    }
}
