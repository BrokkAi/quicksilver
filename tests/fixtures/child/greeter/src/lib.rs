/// Only the example program calls this. A test can see a mutant here only
/// through a child process built from the workspace source.
pub fn greeting() -> String {
    "hello".to_string()
}
