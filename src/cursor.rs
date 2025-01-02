use super::Error;
use futures_util::Future;
use serde::{Deserialize, Serialize};
use std::{pin::Pin, task::Poll};

/// Iterates over the contents of a cursor provided by one of the `open_cursor` functions.
/// You can iterate over it like an async iterator / stream:
/// ```no_run
/// while let Some(entry) = cursor.next().await {
///   // ...
/// }
/// ```
/// or manually iterate, granting access to functions to update or delete
/// the database entry the cursor is during iteration:
/// ```ignore
/// while let Some(entry) = cursor.value()? {
///   //let entry = cursor.update_value(new_value).await?;
///   //cursor.delete_value().await?;
///   cursor.advance().await?;
/// }
/// ```
pub struct Cursor<V> {
	cursor: Option<idb::Cursor>,
	marker: std::marker::PhantomData<V>,
	pending: Option<Pin<Box<dyn Future<Output = Result<(Option<idb::Cursor>, wasm_bindgen::JsValue), idb::Error>>>>>,
}

impl<V> Cursor<V> {
	pub fn new(cursor: Option<idb::Cursor>) -> Self {
		Self {
			cursor,
			marker: Default::default(),
			pending: None,
		}
	}

	pub async fn update_value(&self, new_value: &V) -> Result<(), Error>
	where
		V: Serialize + for<'de> Deserialize<'de>,
	{
		let Some(cursor) = &self.cursor else {
			return Ok(());
		};
		let js_value = serde_wasm_bindgen::to_value(new_value)?;
		cursor.update(&js_value)?.await?;
		Ok(())
	}

	pub async fn delete_value(&self) -> Result<(), idb::Error> {
		if let Some(cursor) = &self.cursor {
			cursor.delete()?.await?;
		}
		Ok(())
	}
}

impl<V> futures_util::stream::Stream for Cursor<V>
where
	V: for<'de> Deserialize<'de> + Unpin,
{
	type Item = V;

	fn poll_next(mut self: Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> Poll<Option<Self::Item>> {
		// Find the pending query
		let mut pending = match self.pending.take() {
			// existing pending query means an operation took longer than immediate
			Some(pending) => pending,
			// the first poll and any poll following a successful first result,
			// will have an invalid pending in this structure.
			None => match self.cursor.take() {
				// an empty cursor on first poll means no elements
				None => return Poll::Ready(None),
				// Construct the new pending operation using the stashed cursor.
				Some(cursor) => Box::pin(async move {
					// move the cursor in so this future can have a static lifetime
					let cursor = cursor;
					// due to changes in idb crate, we must advance to the next entry
					// before determining if the current entry is not a duplicate (the end of the cursor).
					let js_value = cursor.value()?;
					// advance to the next entry
					let adv_request = cursor.advance(1);
					// if this causes and advancement failure, then we've reached the end of the cursor
					if let Err(idb::Error::CursorAdvanceFailed(_)) = &adv_request {
						return Ok((None, js_value));
					}
					// other errors must be bubbled up
					let adv_request = adv_request?;
					let adv_request = adv_request.await;
					// if this causes and advancement failure, then we've reached the end of the cursor
					if let Err(idb::Error::CursorAdvanceFailed(_)) = &adv_request {
						return Ok((None, js_value));
					}
					// other errors must be bubbled up
					let cursor = adv_request?;
					// no errors during advancement, return the current entry and the cursor pointing to the next entry
					Ok((cursor, js_value))
				}),
			},
		};

		// Process any pending advancement future first.
		// If there is a future here, it means we are waiting for the underlying IDB cursor
		// to finish advancing before parsing the current value.
		// This operation may have been from a previous poll, or was just created above.
		let js_value = match pending.as_mut().poll(cx) {
			// the cursor is still advancing, poll the stream later
			Poll::Pending => {
				self.pending = Some(pending);
				return Poll::Pending;
			}
			// found an error either getting a value or advancing to the next item
			Poll::Ready(Err(err)) => {
				log::error!(target: "cursor", "Failed to query next entry from cursor: {err:?}");
				return Poll::Ready(None);
			}
			// we found a value; the next cursor and the current value are provided from the query
			Poll::Ready(Ok((cursor, value))) => {
				self.cursor = cursor;
				value
			}
		};

		// Value is empty, so we've reached end-of-stream.
		if js_value.is_null() {
			return Poll::Ready(None);
		}

		// Parse the valid JSValue as the desired struct type.
		let value = match serde_wasm_bindgen::from_value::<V>(js_value) {
			Ok(value) => value,
			Err(err) => {
				log::error!(target: "cursor", "Failed to parse database value: {err:?}");
				return Poll::Ready(None);
			}
		};

		// Return the found value, while advancement run in the background.
		return Poll::Ready(Some(value));
	}
}
