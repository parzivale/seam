/// Something was refused, and handed back: a task a full fixed-capacity set
/// had no room for (`SpawnInto`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rejected<T>(pub T);
