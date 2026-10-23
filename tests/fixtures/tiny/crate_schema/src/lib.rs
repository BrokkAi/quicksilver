pub fn add(a: i32, b: i32) -> i32 {
    a + b
}

pub fn identity<T>(x: T) -> T {
    x
}

pub struct Unique(pub i32);

pub fn unique() -> Unique {
    Unique(7)
}

pub fn as_ref(x: &i32) -> &i32 {
    x
}

pub fn as_mut(x: &mut i32) -> &mut i32 {
    x
}

pub fn as_impl() -> impl std::fmt::Display {
    42
}

pub fn as_dyn() -> &'static dyn std::fmt::Debug {
    &()
}

pub async fn add_async(a: i32, b: i32) -> i32 {
    a + b
}

#[cfg(windows)]
pub fn windows_only() -> u32 {
    1
}

#[cfg(not(windows))]
pub fn not_windows() -> u32 {
    2
}

pub fn parse_pair(s: &str) -> (i32, i32) {
    struct Pair(i32, i32);
    impl Pair {
        fn parse(s: &str) -> Pair {
            let mut it = s.split(',').map(|x| x.trim().parse::<i32>().unwrap());
            Pair(it.next().unwrap(), it.next().unwrap())
        }
    }
    let p = Pair::parse(s);
    (p.0, p.1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn poll_ready<F: std::future::Future>(fut: F) -> F::Output {
        use std::pin::pin;
        use std::task::{Context, Poll, RawWaker, RawWakerVTable, Waker};
        fn dummy(_: *const ()) {}
        fn clone(_: *const ()) -> RawWaker {
            raw()
        }
        fn raw() -> RawWaker {
            const VTABLE: RawWakerVTable = RawWakerVTable::new(clone, dummy, dummy, dummy);
            RawWaker::new(std::ptr::null(), &VTABLE)
        }
        let waker = unsafe { Waker::from_raw(raw()) };
        let mut cx = Context::from_waker(&waker);
        let mut fut = pin!(fut);
        match fut.as_mut().poll(&mut cx) {
            Poll::Ready(v) => v,
            Poll::Pending => panic!("pending"),
        }
    }

    #[test]
    fn add_works() {
        assert_eq!(add(2, 3), 5);
    }

    #[test]
    fn identity_works() {
        assert_eq!(identity(3), 3);
    }

    #[test]
    fn unique_is_seven() {
        assert_eq!(unique().0, 7);
    }

    #[test]
    fn refs_work() {
        let mut x = 1;
        assert_eq!(*as_ref(&x), 1);
        *as_mut(&mut x) = 2;
        assert_eq!(x, 2);
    }

    #[test]
    fn impl_and_dyn_work() {
        assert_eq!(format!("{}", as_impl()), "42");
        let _ = format!("{:?}", as_dyn());
    }

    #[test]
    fn parse_pair_works() {
        assert_eq!(parse_pair("1, 2"), (1, 2));
    }

    #[test]
    fn add_async_works() {
        assert_eq!(poll_ready(add_async(1, 2)), 3);
    }
}
