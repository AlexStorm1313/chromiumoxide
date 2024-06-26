use std::sync::Arc;

use chromiumoxide_cdp::cdp::browser_protocol::browser::{GetVersionParams, GetVersionReturns};
use chromiumoxide_cdp::cdp::browser_protocol::dom::{
	DiscardSearchResultsParams, GetDocumentParams, GetDocumentReturns, GetSearchResultsParams,
	NodeId, PerformSearchParams, QuerySelectorAllParams, QuerySelectorParams, Rect,
	RemoveNodeParams, RemoveNodeReturns, Rgba, ScrollIntoViewIfNeededParams,
	ScrollIntoViewIfNeededReturns,
};
use chromiumoxide_cdp::cdp::browser_protocol::emulation::{
	ClearDeviceMetricsOverrideParams, ClearDeviceMetricsOverrideReturns,
	SetDefaultBackgroundColorOverrideParams, SetDefaultBackgroundColorOverrideReturns,
	SetDeviceMetricsOverrideParams, SetDeviceMetricsOverrideReturns,
};
use chromiumoxide_cdp::cdp::browser_protocol::input::{
	DispatchKeyEventParams, DispatchKeyEventParamsBuilder, DispatchKeyEventType,
	DispatchMouseEventParams, DispatchMouseEventType, MouseButton,
};
use chromiumoxide_cdp::cdp::browser_protocol::page::{
	CaptureScreenshotParams, FrameId, GetFrameTreeParams, GetFrameTreeReturns,
	GetLayoutMetricsParams, GetLayoutMetricsReturns, ScreencastFrameAckParams,
	ScreencastFrameAckReturns, StartScreencastParams, StartScreencastReturns, StopScreencastParams,
	StopScreencastReturns,
};
use chromiumoxide_cdp::cdp::browser_protocol::target::{ActivateTargetParams, SessionId, TargetId};
use chromiumoxide_cdp::cdp::js_protocol::runtime::{
	CallFunctionOnParams, CallFunctionOnReturns, EvaluateParams, ExecutionContextId, RemoteObjectId,
};
use chromiumoxide_types::{Command, CommandResponse};
use futures::channel::mpsc::{channel, Receiver, Sender};
use futures::channel::oneshot::channel as oneshot_channel;
use futures::future::{join_all, try_join_all};
use futures::stream::Fuse;
use futures::{FutureExt, SinkExt, StreamExt};
use tracing::debug;

use crate::cmd::{to_command_response, CommandMessage};
use crate::error::{CdpError, Result};
use crate::handler::commandfuture::CommandFuture;
use crate::handler::domworld::DOMWorldKind;
use crate::handler::httpfuture::HttpFuture;
use crate::handler::target::{GetExecutionContext, TargetMessage};
use crate::handler::target_message_future::TargetMessageFuture;
use crate::js::EvaluationResult;
use crate::keys::{KeyDefinition, KeyDefinitions};
use crate::layout::Point;
use crate::{keys, utils, ArcHttpRequest};

#[derive(Debug)]
pub struct PageHandle {
	pub(crate) rx: Fuse<Receiver<TargetMessage>>,
	page: Arc<PageInner>,
}

impl PageHandle {
	pub fn new(target_id: TargetId, session_id: SessionId) -> Self {
		let (commands, rx) = channel(1);
		let page = PageInner {
			target_id,
			session_id,
			sender: commands,
		};
		Self {
			rx: rx.fuse(),
			page: Arc::new(page),
		}
	}

	pub(crate) fn inner(&self) -> &Arc<PageInner> {
		&self.page
	}
}

#[derive(Debug)]
pub(crate) struct PageInner {
	target_id: TargetId,
	session_id: SessionId,
	sender: Sender<TargetMessage>,
}

impl PageInner {
	/// Execute a PDL command and return its response
	pub(crate) async fn execute<T: Command>(&self, cmd: T) -> Result<CommandResponse<T::Response>> {
		execute(cmd, self.sender.clone(), Some(self.session_id.clone())).await
	}

	/// Create a PDL command future
	pub(crate) fn command_future<T: Command>(&self, cmd: T) -> Result<CommandFuture<T>> {
		CommandFuture::new(cmd, self.sender.clone(), Some(self.session_id.clone()))
	}

	/// This creates navigation future with the final http response when the page is loaded
	pub(crate) fn wait_for_navigation(&self) -> TargetMessageFuture<ArcHttpRequest> {
		TargetMessageFuture::<ArcHttpRequest>::wait_for_navigation(self.sender.clone())
	}

	/// This creates HTTP future with navigation and responds with the final
	/// http response when the page is loaded
	pub(crate) fn http_future<T: Command>(&self, cmd: T) -> Result<HttpFuture<T>> {
		Ok(HttpFuture::new(
			self.sender.clone(),
			self.command_future(cmd)?,
		))
	}

	/// The identifier of this page's target
	pub fn target_id(&self) -> &TargetId {
		&self.target_id
	}

	/// The identifier of this page's target's session
	pub fn session_id(&self) -> &SessionId {
		&self.session_id
	}

	pub(crate) fn sender(&self) -> &Sender<TargetMessage> {
		&self.sender
	}

	/// Activates (focuses) the target.
	pub async fn activate(&self) -> Result<&Self> {
		self.execute(ActivateTargetParams::new(self.target_id().clone()))
			.await?;
		Ok(self)
	}

	/// Version information about the browser
	pub async fn version(&self) -> Result<GetVersionReturns> {
		Ok(self.execute(GetVersionParams::default()).await?.result)
	}

	/// Returns the first element in the node which matches the given CSS
	/// selector.
	pub async fn query_selector(
		&self,
		selector: impl Into<String>,
		node: NodeId,
	) -> Result<NodeId> {
		Ok(self
			.execute(QuerySelectorParams::new(node, selector))
			.await?
			.node_id)
	}

	/// Return all `Element`s inside the node that match the given selector
	pub async fn query_selector_all(
		&self,
		selector: impl Into<String>,
		node: NodeId,
	) -> Result<Vec<NodeId>> {
		Ok(self
			.execute(QuerySelectorAllParams::new(node, selector))
			.await?
			.result
			.node_ids)
	}

	/// Returns all elements which matches the given plain text, query selector or xpath search query
	pub async fn perfrom_search(&self, query: impl Into<String>) -> Result<Vec<NodeId>> {
		let perform_search_returns = self
			.execute(PerformSearchParams {
				query: query.into(),
				include_user_agent_shadow_dom: Some(true),
			})
			.await?
			.result;

		let search_results = self
			.execute(GetSearchResultsParams::new(
				perform_search_returns.search_id.clone(),
				0,
				perform_search_returns.result_count,
			))
			.await?
			.result;

		self.execute(DiscardSearchResultsParams::new(
			perform_search_returns.search_id,
		))
		.await?;

		Ok(search_results.node_ids)
	}

	/// Remove Element from document
	pub async fn remove(&self, node_id: NodeId) -> Result<RemoveNodeReturns> {
		Ok(self
			.execute(
				RemoveNodeParams::builder()
					.node_id(node_id)
					.build()
					.unwrap(),
			)
			.await?
			.result)
	}

	/// Moves the mouse to this point (dispatches a mouseMoved event)
	pub async fn move_mouse(&self, point: Point) -> Result<&Self> {
		self.execute(DispatchMouseEventParams::new(
			DispatchMouseEventType::MouseMoved,
			point.x,
			point.y,
		))
		.await?;
		Ok(self)
	}

	/// Performs a mouse click event at the point's location
	pub async fn click(&self, point: Point) -> Result<&Self> {
		let cmd = DispatchMouseEventParams::builder()
			.x(point.x)
			.y(point.y)
			.button(MouseButton::Left)
			.click_count(1);

		self.move_mouse(point)
			.await?
			.execute(
				cmd.clone()
					.r#type(DispatchMouseEventType::MousePressed)
					.build()
					.unwrap(),
			)
			.await?;

		self.execute(
			cmd.r#type(DispatchMouseEventType::MouseReleased)
				.build()
				.unwrap(),
		)
		.await?;

		Ok(self)
	}

	pub async fn scroll(&self, rect: impl Into<Rect>) -> Result<ScrollIntoViewIfNeededReturns> {
		Ok(self
			.execute(
				ScrollIntoViewIfNeededParams::builder()
					.backend_node_id(self.get_document().await?.root.backend_node_id)
					.rect(rect)
					.build(),
			)
			.await?
			.result)
	}

	/// This simulates pressing keys on the page.
	///
	/// # Note The `input` is treated as series of `KeyDefinition`s, where each
	/// char is inserted as a separate keystroke. So sending
	/// `page.type_str("Enter")` will be processed as a series of single
	/// keystrokes:  `["E", "n", "t", "e", "r"]`. To simulate pressing the
	/// actual Enter key instead use `page.press_key(
	/// keys::get_key_definition("Enter").unwrap())`.
	pub async fn type_str(&self, input: impl AsRef<str>) -> Result<&Self> {
		for c in input.as_ref().split("").filter(|s| !s.is_empty()) {
			self.press_key(c).await?;
		}
		Ok(self)
	}

	pub async fn keys_down(&self, keys: Vec<impl AsRef<str>>) -> Result<&Self> {
		let key_defenitions = KeyDefinitions(
			keys.iter()
				.map(|key| keys::get_key_definition(key).unwrap())
				.collect::<Vec<KeyDefinition>>(),
		);
		let events = Into::<Vec<DispatchKeyEventParamsBuilder>>::into(key_defenitions);
		let fut = events
			.into_iter()
			.map(|disp| self.execute(disp.build().unwrap()));

		try_join_all(fut).await?;

		Ok(self)
	}

	pub async fn keys_up(&self, keys: Vec<impl AsRef<str>>) -> Result<&Self> {
		let key_defenitions = KeyDefinitions(
			keys.iter()
				.map(|key| keys::get_key_definition(key).unwrap())
				.collect::<Vec<KeyDefinition>>(),
		);
		let events = Into::<Vec<DispatchKeyEventParamsBuilder>>::into(key_defenitions);
		let fut = events
			.into_iter()
			.map(|disp| self.execute(disp.r#type(DispatchKeyEventType::KeyUp).build().unwrap()));

		try_join_all(fut).await?;

		Ok(self)
	}

	/// Uses the `DispatchKeyEvent` mechanism to simulate pressing keyboard
	/// keys.
	pub async fn press_key(&self, key: impl AsRef<str>) -> Result<&Self> {
		self.execute(
			Into::<DispatchKeyEventParamsBuilder>::into(keys::get_key_definition(key.as_ref())?)
				.build()
				.unwrap(),
		)
		.await?;

		self.execute(
			Into::<DispatchKeyEventParamsBuilder>::into(keys::get_key_definition(key.as_ref())?)
				.r#type(DispatchKeyEventType::KeyUp)
				.build()
				.unwrap(),
		)
		.await?;

		Ok(self)
	}

	/// Calls function with given declaration on the remote object with the
	/// matching id
	pub async fn call_js_fn(
		&self,
		function_declaration: impl Into<String>,
		await_promise: bool,
		remote_object_id: RemoteObjectId,
	) -> Result<CallFunctionOnReturns> {
		let resp = self
			.execute(
				CallFunctionOnParams::builder()
					.object_id(remote_object_id)
					.function_declaration(function_declaration)
					.generate_preview(true)
					.await_promise(await_promise)
					.build()
					.unwrap(),
			)
			.await?;
		Ok(resp.result)
	}

	pub async fn evaluate_expression(
		&self,
		evaluate: impl Into<EvaluateParams>,
	) -> Result<EvaluationResult> {
		let mut evaluate = evaluate.into();
		if evaluate.context_id.is_none() {
			evaluate.context_id = self.execution_context().await?;
		}
		if evaluate.await_promise.is_none() {
			evaluate.await_promise = Some(true);
		}
		if evaluate.return_by_value.is_none() {
			evaluate.return_by_value = Some(true);
		}

		let resp = self.execute(evaluate).await?.result;
		if let Some(exception) = resp.exception_details {
			return Err(CdpError::JavascriptException(Box::new(exception)));
		}

		Ok(EvaluationResult::new(resp.result))
	}

	pub async fn evaluate_function(
		&self,
		evaluate: impl Into<CallFunctionOnParams>,
	) -> Result<EvaluationResult> {
		let mut evaluate = evaluate.into();
		if evaluate.execution_context_id.is_none() {
			evaluate.execution_context_id = self.execution_context().await?;
		}
		if evaluate.await_promise.is_none() {
			evaluate.await_promise = Some(true);
		}
		if evaluate.return_by_value.is_none() {
			evaluate.return_by_value = Some(true);
		}

		let resp = self.execute(evaluate).await?.result;
		if let Some(exception) = resp.exception_details {
			return Err(CdpError::JavascriptException(Box::new(exception)));
		}
		Ok(EvaluationResult::new(resp.result))
	}

	pub async fn execution_context(&self) -> Result<Option<ExecutionContextId>> {
		self.execution_context_for_world(None, DOMWorldKind::Main)
			.await
	}

	pub async fn secondary_execution_context(&self) -> Result<Option<ExecutionContextId>> {
		self.execution_context_for_world(None, DOMWorldKind::Secondary)
			.await
	}

	pub async fn frame_execution_context(
		&self,
		frame_id: FrameId,
	) -> Result<Option<ExecutionContextId>> {
		self.execution_context_for_world(Some(frame_id), DOMWorldKind::Main)
			.await
	}

	pub async fn frame_secondary_execution_context(
		&self,
		frame_id: FrameId,
	) -> Result<Option<ExecutionContextId>> {
		self.execution_context_for_world(Some(frame_id), DOMWorldKind::Secondary)
			.await
	}

	pub async fn execution_context_for_world(
		&self,
		frame_id: Option<FrameId>,
		dom_world: DOMWorldKind,
	) -> Result<Option<ExecutionContextId>> {
		let (tx, rx) = oneshot_channel();
		self.sender
			.clone()
			.send(TargetMessage::GetExecutionContext(GetExecutionContext {
				dom_world,
				frame_id,
				tx,
			}))
			.await?;
		Ok(rx.await?)
	}

	/// Returns metrics relating to the layout of the page
	pub async fn layout_metrics(&self) -> Result<GetLayoutMetricsReturns> {
		Ok(self
			.execute(GetLayoutMetricsParams::default())
			.await?
			.result)
	}

	/// Returns metrics relating to the layout of the page
	pub async fn frame_tree(&self) -> Result<GetFrameTreeReturns> {
		Ok(self.execute(GetFrameTreeParams::default()).await?.result)
	}

	/// Returns the root DOM node (and optionally the subtree) of the page.
	///
	/// # Note: This does not return the actual HTML document of the page. To
	/// retrieve the HTML content of the page see `Page::content`.
	pub async fn get_document(&self) -> Result<GetDocumentReturns> {
		// Ok(self.execute(GetDocumentParams::default()).await?.result)
		Ok(self
			.execute(GetDocumentParams {
				depth: Some(-1),
				pierce: Some(true),
			})
			.await?
			.result)
	}

	/// Screenshot function for CaptureScreenshot command
	pub async fn screenshot(
		&self,
		capture_screenshot_params: CaptureScreenshotParams,
	) -> Result<Vec<u8>> {
		Ok(utils::base64::decode(
			&self.execute(capture_screenshot_params).await?.result.data,
		)?)
	}

	pub async fn start_screencast(
		&self,
		start_screencast_params: StartScreencastParams,
	) -> Result<StartScreencastReturns> {
		Ok(self.execute(start_screencast_params).await?.result)
	}

	pub async fn stop_screencast(
		&self,
		stop_screencast_params: StopScreencastParams,
	) -> Result<StopScreencastReturns> {
		Ok(self.execute(stop_screencast_params).await?.result)
	}

	pub async fn screencast_frame_ack(
		&self,
		screencast_frame_ack_params: ScreencastFrameAckParams,
	) -> Result<ScreencastFrameAckReturns> {
		Ok(self.execute(screencast_frame_ack_params).await?.result)
	}

	/// Overrides the background color of the page, usefull when taking screenshots
	pub async fn set_background_color(
		&self,
		color: Option<Rgba>,
	) -> Result<SetDefaultBackgroundColorOverrideReturns> {
		Ok(self
			.execute(SetDefaultBackgroundColorOverrideParams { color })
			.await?
			.result)
	}

	/// Overrides the current device emultation
	pub async fn set_device_metrics(
		&self,
		device_metrics: impl Into<SetDeviceMetricsOverrideParams>,
	) -> Result<SetDeviceMetricsOverrideReturns> {
		Ok(self.execute(device_metrics.into()).await?.result)
	}

	/// Clears the current device emultation back to default
	pub async fn clear_device_metrics(&self) -> Result<ClearDeviceMetricsOverrideReturns> {
		Ok(self
			.execute(ClearDeviceMetricsOverrideParams {})
			.await?
			.result)
	}
}

pub(crate) async fn execute<T: Command>(
	cmd: T,
	mut sender: Sender<TargetMessage>,
	session: Option<SessionId>,
) -> Result<CommandResponse<T::Response>> {
	let (tx, rx) = oneshot_channel();
	let method = cmd.identifier();
	let msg = CommandMessage::with_session(cmd, tx, session)?;

	sender.send(TargetMessage::Command(msg)).await?;
	let resp = rx.await??;
	to_command_response::<T>(resp, method)
}
