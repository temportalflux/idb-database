pub trait Schema {
	fn latest() -> u32;
	fn apply(&self, database: &crate::Client) -> Result<(), crate::Error>;
}
