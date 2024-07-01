use std::path::{Path, PathBuf};
use std::{env, fs};

pub mod build;
pub mod pdl;

// pub fn download_pdl() {
// 	fs::create_dir_all("pdl");
// 	let dir = Path::new("pdl");

// 	let js_proto_new = ureq::get(&format!(
// 		"https://raw.githubusercontent.com/ChromeDevTools/devtools-protocol/{}/pdl/js_protocol.pdl",
// 		"master"
// 	))
// 	.call()
// 	.unwrap()
// 	.into_string()
// 	.unwrap();
// 	assert!(js_proto_new.contains("The Chromium Authors"));

// 	let browser_proto_new = ureq::get(&format!("https://raw.githubusercontent.com/ChromeDevTools/devtools-protocol/{}/pdl/browser_protocol.pdl", "master"))
//         .call()
//         .unwrap()
//         .into_string()
//         .unwrap();
// 	assert!(browser_proto_new.contains("The Chromium Authors"));

// 	fs::write(dir.join("js_protocol.pdl"), js_proto_new).unwrap();
// 	fs::write(dir.join("browser_protocol.pdl"), browser_proto_new).unwrap();
// }
