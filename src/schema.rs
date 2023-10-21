pub trait Schema {
	fn latest() -> u32;
	fn apply(&self, database: &crate::Client, transaction: Option<&crate::Transaction>) -> Result<(), crate::Error>;
}
