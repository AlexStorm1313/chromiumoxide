use std::collections::HashMap;
use std::path::Path;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use chromiumoxide_cdp::cdp::browser_protocol;
use chromiumoxide_cdp::cdp::browser_protocol::dom::{
	self, BackendNodeId, DescribeNodeParams, FocusParams, FocusParamsBuilder, GetBoxModelParams,
	GetContentQuadsParams, Node, NodeId, RemoveNodeReturns, ResolveNodeParams,
	ScrollIntoViewIfNeededParams,
};
use chromiumoxide_cdp::cdp::browser_protocol::page::Viewport;
use chromiumoxide_cdp::cdp::js_protocol::runtime::{
	CallFunctionOnReturns, GetPropertiesParams, PropertyDescriptor, RemoteObjectId,
	RemoteObjectType,
};
use futures::{future, Future, FutureExt, Stream};
use tracing::debug;

use crate::error::{CdpError, Result};
use crate::handler::PageInner;
use crate::layout::{BoundingBox, BoxModel, ElementQuad, Point};
use crate::{page, utils};

/// Represents a [DOM Element](https://developer.mozilla.org/en-US/docs/Web/API/Element).
#[derive(Debug)]
pub struct Element {
	/// The Unique object identifier
	pub remote_object_id: RemoteObjectId,
	/// Identifier of the backend node.
	pub backend_node_id: BackendNodeId,
	/// The identifier of the node this element represents.
	pub node_id: NodeId,
	tab: Arc<PageInner>,
}

impl Element {
	pub(crate) async fn new(tab: Arc<PageInner>, node_id: NodeId) -> Result<Self> {
		let backend_node_id = tab
			.execute(
				DescribeNodeParams::builder()
					.node_id(node_id)
					.depth(-1)
					.build(),
			)
			.await?
			.node
			.backend_node_id;

		let resp = tab
			.execute(
				ResolveNodeParams::builder()
					.backend_node_id(backend_node_id)
					.build(),
			)
			.await?;

		let remote_object_id = resp
			.result
			.object
			.object_id
			.ok_or_else(|| CdpError::msg(format!("No object Id found for {node_id:?}")))?;
		Ok(Self {
			remote_object_id,
			backend_node_id,
			node_id,
			tab,
		})
	}

	/// Convert a slice of `NodeId`s into a `Vec` of `Element`s
	pub(crate) async fn from_nodes(tab: &Arc<PageInner>, node_ids: &[NodeId]) -> Result<Vec<Self>> {
		future::join_all(
			node_ids
				.iter()
				.copied()
				.map(|id| Element::new(Arc::clone(tab), id)),
		)
		.await
		.into_iter()
		.collect::<Result<Vec<_>, _>>()
	}

	/// Remove Element from document
	pub async fn remove(&self) -> Result<RemoveNodeReturns> {
		Ok(self.tab.remove(self.node_id).await?)
	}

	/// Return first Element in the document that match the given selector
	pub async fn query_element(&self, selector: &String) -> Result<Element> {
		Element::new(
			self.tab.clone(),
			self.tab.query_selector(selector, self.node_id).await?,
		)
		.await
	}

	/// Return all Elements in the document that match the given selector
	pub async fn query_elements(&self, selector: &String) -> Result<Vec<Element>> {
		Element::from_nodes(
			&self.tab,
			&self.tab.query_selector_all(selector, self.node_id).await?,
		)
		.await
	}

	async fn box_model(&self) -> Result<BoxModel> {
		let model = self
			.tab
			.execute(
				GetBoxModelParams::builder()
					.backend_node_id(self.backend_node_id)
					.build(),
			)
			.await?
			.result
			.model;
		Ok(BoxModel {
			content: ElementQuad::from_quad(&model.content),
			padding: ElementQuad::from_quad(&model.padding),
			border: ElementQuad::from_quad(&model.border),
			margin: ElementQuad::from_quad(&model.margin),
			width: model.width as u32,
			height: model.height as u32,
		})
	}

	/// Returns the bounding box of the element (relative to the main frame)
	pub async fn bounding_box(&self) -> Result<BoundingBox> {
		let bounds = self.box_model().await?;
		let quad = bounds.border;

		let x = quad.most_left();
		let y = quad.most_top();
		let width = quad.most_right() - x;
		let height = quad.most_bottom() - y;

		Ok(BoundingBox {
			x,
			y,
			width,
			height,
		})
	}

	/// Returns the best `Point` of this node to execute a click on.
	pub async fn clickable_point(&self) -> Result<Point> {
		let content_quads = self
			.tab
			.execute(
				GetContentQuadsParams::builder()
					.backend_node_id(self.backend_node_id)
					.build(),
			)
			.await?;
		content_quads
			.quads
			.iter()
			.filter(|q| q.inner().len() == 8)
			.map(ElementQuad::from_quad)
			.filter(|q| q.quad_area() > 1.)
			.map(|q| q.quad_center())
			.next()
			.ok_or_else(|| CdpError::msg("Node is either not visible or not an HTMLElement"))
	}

	/// Submits a javascript function to the page and returns the evaluated
	/// result
	///
	/// # Example get the element as JSON object
	///
	/// ```no_run
	/// # use chromiumoxide::element::Element;
	/// # use chromiumoxide::error::Result;
	/// # async fn demo(element: Element) -> Result<()> {
	///     let js_fn = "function() { return this; }";
	///     let element_json = element.call_js_fn(js_fn, false).await?;
	///     # Ok(())
	/// # }
	/// ```
	///
	/// # Execute an async javascript function
	///
	/// ```no_run
	/// # use chromiumoxide::element::Element;
	/// # use chromiumoxide::error::Result;
	/// # async fn demo(element: Element) -> Result<()> {
	///     let js_fn = "async function() { return this; }";
	///     let element_json = element.call_js_fn(js_fn, true).await?;
	///     # Ok(())
	/// # }
	/// ```
	pub async fn call_js_fn(
		&self,
		function_declaration: impl Into<String>,
		await_promise: bool,
	) -> Result<CallFunctionOnReturns> {
		self.tab
			.call_js_fn(
				function_declaration,
				await_promise,
				self.remote_object_id.clone(),
			)
			.await
	}

	/// Returns a JSON representation of this element.
	pub async fn json_value(&self) -> Result<serde_json::Value> {
		let element_json = self
			.call_js_fn("function() { return this; }", false)
			.await?;
		element_json.result.value.ok_or(CdpError::NotFound)
	}

	/// Focus element using Devtools native functions
	pub async fn focus(&self) -> Result<&Self> {
		self.scroll_into_view().await?;
		self.tab
			.execute(
				FocusParams::builder()
					.backend_node_id(self.backend_node_id)
					.build(),
			)
			.await?;

		Ok(&self)
	}

	/// Calls [focus](https://developer.mozilla.org/en-US/docs/Web/API/HTMLElement/focus) on the element via JavaScript.
	pub async fn focus_js(&self) -> Result<&Self> {
		self.call_js_fn("function() { this.focus(); }", true)
			.await?;
		Ok(self)
	}

	/// Scrolls the element into view and uses a mouse event to move the mouse
	/// over the center of this element.
	pub async fn hover(&self) -> Result<&Self> {
		self.scroll_into_view().await?;
		self.tab.move_mouse(self.clickable_point().await?).await?;
		Ok(self)
	}

	/// Scrolls the element into view.
	///
	/// Fails if the element's node is not a HTML element or is detached from
	/// the document
	pub async fn scroll_into_view_js(&self) -> Result<&Self> {
		let resp = self
			.call_js_fn(
				"async function() {
                if (!this.isConnected)
                    return 'Node is detached from document';
                if (this.nodeType !== Node.ELEMENT_NODE)
                    return 'Node is not of type HTMLElement';

                const visibleRatio = await new Promise(resolve => {
                    const observer = new IntersectionObserver(entries => {
                        resolve(entries[0].intersectionRatio);
                        observer.disconnect();
                    });
                    observer.observe(this);
                });

                if (visibleRatio !== 1.0)
                    this.scrollIntoView({
                        block: 'center',
                        inline: 'center',
                        behavior: 'smooth'
                    });
                return false;
            }",
				true,
			)
			.await?;

		if resp.result.r#type == RemoteObjectType::String {
			let error_text = resp.result.value.unwrap().as_str().unwrap().to_string();
			return Err(CdpError::ScrollingFailed(error_text));
		}
		Ok(self)
	}

	/// Scrolls the element into view.
	///
	/// Fails if the element's node is not a HTML element or is detached from
	/// the document
	pub async fn scroll_into_view(&self) -> Result<&Self> {
		self.tab
			.execute(
				ScrollIntoViewIfNeededParams::builder()
					.backend_node_id(self.backend_node_id)
					.build(),
			)
			.await?;
		Ok(self)
	}

	/// This focuses the element by click on it
	///
	/// Bear in mind that if `click()` triggers a navigation this element may be
	/// not exist anymore.
	pub async fn click(&self) -> Result<&Self> {
		let center = self.scroll_into_view().await?.clickable_point().await?;
		self.tab.click(center).await?;
		Ok(self)
	}

	/// Type the input
	///
	/// # Example type text into an input element
	///
	/// ```no_run
	/// # use chromiumoxide::page::Page;
	/// # use chromiumoxide::error::Result;
	/// # async fn demo(page: Page) -> Result<()> {
	///     let element = page.find_element("input#searchInput").await?;
	///     element.click().await?.type_str("this goes into the input field").await?;
	///     # Ok(())
	/// # }
	/// ```
	pub async fn type_str(&self, input: impl AsRef<str>) -> Result<&Self> {
		self.tab.type_str(input).await?;
		Ok(self)
	}

	/// Presses the key.
	///
	/// # Example type text into an input element and hit enter
	///
	/// ```no_run
	/// # use chromiumoxide::page::Page;
	/// # use chromiumoxide::error::Result;
	/// # async fn demo(page: Page) -> Result<()> {
	///     let element = page.find_element("input#searchInput").await?;
	///     element.click().await?.type_str("this goes into the input field").await?
	///          .press_key("Enter").await?;
	///     # Ok(())
	/// # }
	/// ```
	pub async fn press_key(&self, key: impl AsRef<str>) -> Result<&Self> {
		self.tab.press_key(key).await?;
		Ok(self)
	}

	/// The description of the element's node
	pub async fn description(&self) -> Result<Node> {
		Ok(self
			.tab
			.execute(
				DescribeNodeParams::builder()
					// .node_id(self.node_id)
					.backend_node_id(self.backend_node_id)
					.depth(-1)
					.build(),
			)
			.await?
			.result
			.node)
	}

	/// Attributes of the `Element` node in the form of flat array `[name1,
	/// value1, name2, value2]
	pub async fn attributes(&self) -> Result<Vec<String>> {
		let node = self.description().await?;
		Ok(node.attributes.unwrap_or_default())
	}

	/// Attributes of the `Element` node in the form of HasMap
	pub async fn attributes_map(&self) -> Result<HashMap<String, String>> {
		Ok(self
			.description()
			.await?
			.attributes
			.unwrap_or_default()
			.chunks_exact(2)
			.map(|chunk| (chunk[0].clone(), chunk[1].clone()))
			.collect::<HashMap<String, String>>())
	}

	/// Returns the key and value of the element's attribute
	pub async fn attribute(&self, attribute: &str) -> Result<Option<String>> {
		Ok(self.attributes_map().await?.get(attribute).cloned())
	}

	/// Returns the value of the element's attribute
	pub async fn attribute_js(&self, attribute: impl AsRef<str>) -> Result<Option<String>> {
		let js_fn = format!(
			"function() {{ return this.getAttribute('{}'); }}",
			attribute.as_ref()
		);
		let resp = self.call_js_fn(js_fn, false).await?;
		if let Some(value) = resp.result.value {
			Ok(serde_json::from_value(value)?)
		} else {
			Ok(None)
		}
	}

	/// Set element attribute
	pub async fn set_attribute(&self, name: String, value: String) -> Result<&Self> {
		self.tab
			.execute(dom::SetAttributeValueParams {
				node_id: self.node_id,
				name,
				value,
			})
			.await?;

		Ok(&self)
	}

	/// A `Stream` over all attributes and their values
	pub async fn iter_attributes(
		&self,
	) -> Result<impl Stream<Item = (String, Result<Option<String>>)> + '_> {
		let attributes = self.attributes().await?;
		Ok(AttributeStream {
			attributes,
			fut: None,
			element: self,
		})
	}

	pub async fn children(&self) -> Result<Vec<Element>> {
		self.tab
			.execute(dom::RequestChildNodesParams {
				node_id: self.node_id,
				depth: Some(-1),
				pierce: Some(true),
			})
			.await?;
		Ok(Element::from_nodes(
			&self.tab,
			&self
				.description()
				.await?
				.children
				.unwrap_or_default()
				.into_iter()
				.map(|child| child.node_id)
				.collect::<Vec<NodeId>>(),
		)
		.await?)
	}

	pub async fn tag(&self) -> Result<String> {
		Ok(self.description().await?.node_name)
	}

	pub async fn value(&self) -> Result<String> {
		Ok(self.description().await?.node_value)
	}

	/// The inner text of this element.
	pub async fn inner_text(&self) -> Result<Option<String>> {
		self.string_property("innerText").await
	}

	/// The inner HTML of this element.
	pub async fn inner_html(&self) -> Result<Option<String>> {
		self.string_property("innerHTML").await
	}

	/// The outer HTML of this element.
	pub async fn outer_html(&self) -> Result<Option<String>> {
		self.string_property("outerHTML").await
	}

	/// Returns the string property of the element.
	///
	/// If the property is an empty String, `None` is returned.
	pub async fn string_property(&self, property: impl AsRef<str>) -> Result<Option<String>> {
		let property = property.as_ref();
		let value = self.property(property).await?.ok_or(CdpError::NotFound)?;
		let txt: String = serde_json::from_value(value)?;
		if !txt.is_empty() {
			Ok(Some(txt))
		} else {
			Ok(None)
		}
	}

	/// Returns the javascript `property` of this element where `property` is
	/// the name of the requested property of this element.
	///
	/// See also `Element::inner_html`.
	pub async fn property(&self, property: impl AsRef<str>) -> Result<Option<serde_json::Value>> {
		let js_fn = format!("function() {{ return this.{}; }}", property.as_ref());
		let resp = self.call_js_fn(js_fn, false).await?;
		Ok(resp.result.value)
	}

	/// Returns a map with all `PropertyDescriptor`s of this element keyed by
	/// their names
	pub async fn properties(&self) -> Result<HashMap<String, PropertyDescriptor>> {
		let mut params = GetPropertiesParams::new(self.remote_object_id.clone());
		params.own_properties = Some(true);

		let properties = self.tab.execute(params).await?;

		Ok(properties
			.result
			.result
			.into_iter()
			.map(|p| (p.name.clone(), p))
			.collect())
	}

	/// Get the HTML content of the element
	pub async fn get_content(&self) -> Result<String> {
		Ok(self
			.tab
			.execute(browser_protocol::dom::GetOuterHtmlParams {
				node_id: Some(self.node_id),
				backend_node_id: Some(self.backend_node_id),
				object_id: Some(self.remote_object_id.clone()),
			})
			.await?
			.result
			.outer_html)
	}

	/// Overwrite the current content of the element with the supplied HTML
	pub async fn set_content(&self, html: impl AsRef<str>) -> Result<&Self> {
		self.tab
			.execute(browser_protocol::dom::SetOuterHtmlParams {
				node_id: self.node_id,
				outer_html: html.as_ref().to_string(),
			})
			.await?;

		Ok(&self)
	}

	/// Takes a screenshot of the selected Element
	///
	/// Instead of scrolling the element into the Viewport resize the device
	/// to display/render all of the page content. This method still works
	/// even if the element is bigger than the current Viewport
	pub async fn screenshot(
		&self,
		screenshot_params: impl Into<page::ScreenshotParams>,
	) -> Result<Vec<u8>> {
		let screenshot_params: page::ScreenshotParams = screenshot_params.into();

		let mut viewport: Viewport = self.bounding_box().await?.into();
		let mut device_metrics = screenshot_params.device_metrics();
		let mut capture_screenshot_params = screenshot_params.capture_screenshot_params.clone();

		let metrics = self.tab.layout_metrics().await?;

		// Do the image resize magic
		if let Some((width, height)) = screenshot_params.screenshot_dimensions() {
			viewport.scale = (((width as f64 - viewport.width) / viewport.width) + 1.)
				/ device_metrics.device_scale_factor;

			if height != 0 {
				let corrected_height =
					(height as f64 / viewport.scale) / device_metrics.device_scale_factor;

				viewport.y -= (corrected_height / 2.) - (viewport.height / 2.);
				viewport.height = corrected_height;
			}
		}

		// Resize window to load all content on the page
		device_metrics.width = metrics.css_content_size.width as i64;
		device_metrics.height = metrics.css_content_size.height as i64;

		if screenshot_params.omit_background() {
			self.tab
				.set_background_color(Some(dom::Rgba {
					r: 0,
					g: 0,
					b: 0,
					a: Some(0.),
				}))
				.await?;
		}

		capture_screenshot_params.clip = Some(viewport);
		self.tab.set_device_metrics(device_metrics).await?;

		let screenshot = self.tab.screenshot(capture_screenshot_params).await?;

		if screenshot_params.omit_background() {
			self.tab.set_background_color(None).await?;
		}

		self.tab
			.set_device_metrics(screenshot_params.device_metrics())
			.await?;

		Ok(screenshot)
	}

	// Save a screenshot of the element and write it to `output`
	pub async fn save_screenshot(
		&self,
		screenshot_params: impl Into<page::ScreenshotParams>,
		output: impl AsRef<Path>,
	) -> Result<Vec<u8>> {
		let img = self.screenshot(screenshot_params).await?;
		utils::write(output.as_ref(), &img).await?;
		Ok(img)
	}
}

pub type AttributeValueFuture<'a> = Option<(
	String,
	Pin<Box<dyn Future<Output = Result<Option<String>>> + 'a>>,
)>;

/// Stream over all element's attributes
#[must_use = "streams do nothing unless polled"]
#[allow(missing_debug_implementations)]
pub struct AttributeStream<'a> {
	attributes: Vec<String>,
	fut: AttributeValueFuture<'a>,
	element: &'a Element,
}

impl<'a> Stream for AttributeStream<'a> {
	type Item = (String, Result<Option<String>>);

	fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
		let pin = self.get_mut();

		if pin.fut.is_none() {
			if let Some(name) = pin.attributes.pop() {
				let fut = Box::pin(pin.element.attribute_js(name.clone()));
				pin.fut = Some((name, fut));
			} else {
				return Poll::Ready(None);
			}
		}

		if let Some((name, mut fut)) = pin.fut.take() {
			if let Poll::Ready(res) = fut.poll_unpin(cx) {
				return Poll::Ready(Some((name, res)));
			} else {
				pin.fut = Some((name, fut));
			}
		}
		Poll::Pending
	}
}
