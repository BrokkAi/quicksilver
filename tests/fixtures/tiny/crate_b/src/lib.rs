pub fn mul(a: i32, b: i32) -> i32 {
    a * b
}

pub fn uses_flag() -> bool {
    tiny_a::flag()
}

#[cfg(test)]
mod tests {
    #[test]
    fn mul_works() {
        assert_eq!(super::mul(3, 4), 12);
    }
}
