#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i32)]
pub enum EResult {
    Invalid = 0,
    Ok = 1,
    Fail = 2,
}
