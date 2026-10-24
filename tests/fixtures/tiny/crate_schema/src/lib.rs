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

pub fn shout(s: &str) -> String {
    s.to_uppercase()
}

pub fn calls_no_one_tests() -> u32 {
    called_in_production()
}

fn called_in_production() -> u32 {
    5
}

pub fn sample_for_test() -> u32 {
    3
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

    // Stands in for the proptest crate: QS finds properties by the macro's
    // name, and `PROPTEST_CASES` plays the part of more random cases.
    macro_rules! proptest {
        ($(#[test] fn $name:ident() $body:block)*) => { $(#[test] fn $name() $body)* };
    }

    proptest! {
        #[test]
        fn shout_property() {
            assert!(!shout("a").is_empty());
            if std::env::var("PROPTEST_CASES").as_deref() == Ok("1000") {
                assert_eq!(shout("a"), "A");
            }
        }
    }

    #[test]
    fn state_goes_under_a_fresh_home() {
        // Under quicksilver's sandbox each test process has a new HOME, and
        // `[test-env]` puts FIXTURE_STATE in the same sandbox.
        let Ok(state) = std::env::var("FIXTURE_STATE") else {
            return;
        };
        let home = std::path::PathBuf::from(std::env::var("HOME").unwrap());
        assert_eq!(std::path::Path::new(&state).parent(), home.parent());
        let marker = home.join(".cache").join("fixture").join("marker");
        assert!(!marker.exists(), "{} is left over", marker.display());
        std::fs::create_dir_all(marker.parent().unwrap()).unwrap();
        std::fs::write(&marker, add(1, 2).to_string()).unwrap();
        // For git, the temporary directory is in no repository.
        let tmp = std::env::temp_dir();
        if let Ok(out) = std::process::Command::new("git")
            .args(["rev-parse", "--absolute-git-dir"])
            .current_dir(&tmp)
            .output()
        {
            assert!(
                !out.status.success(),
                "{} is inside the repository at {}",
                tmp.display(),
                String::from_utf8_lossy(&out.stdout).trim()
            );
        }
    }

    #[test]
    fn red_only_when_asked() {
        // Quicksilver's end-to-end test sets this through `[test-env]`, to
        // check that a test that is not green keeps its output.
        if std::env::var_os("FIXTURE_NOT_GREEN").is_some() {
            println!("output before the failure");
            panic!("asked to fail by FIXTURE_NOT_GREEN");
        }
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
