// CEF subprocess entry point (renderer, GPU, utility, …).
//
// For the renderer sub-process we install a `RenderProcessHandler` that
// natively swaps `window.Notification` at V8 context creation time and
// forwards notification invocations to the browser process over CEF IPC
// (`openhuman.notification.show`). The browser-side code in
// `tauri-runtime-cef/src/cef_impl.rs` picks up the message and fans out to
// the per-webview `NotificationHandler`.
//
// This file is copied into OUT_DIR at build time by tauri-bundler's build.rs,
// so everything must live in main.rs — no extra modules, no extra deps beyond
// what cef-helper/Cargo.toml already declares.

use cef::{args::Args, rc::*, sys::cef_v8_propertyattribute_t, *};

const IPC_NOTIFICATION_SHOW: &str = "openhuman.notification.show";
const IPC_PERMISSION_REQUEST: &str = "openhuman.notification.permission_request";

fn read_string_prop(obj: &V8Value, key: &str) -> Option<String> {
  let k = CefString::from(key);
  if obj.has_value_bykey(Some(&k)) == 0 {
    return None;
  }
  let v = obj.value_bykey(Some(&k))?;
  if v.is_string() == 0 {
    return None;
  }
  let s = CefString::from(&v.string_value()).to_string();
  if s.is_empty() { None } else { Some(s) }
}

fn origin_from(url: &str) -> String {
  if let Some(scheme_end) = url.find("://") {
    let rest = &url[scheme_end + 3..];
    let host_end = rest.find('/').unwrap_or(rest.len());
    return format!("{}{}", &url[..scheme_end + 3], &rest[..host_end]);
  }
  String::new()
}

fn send_notification_ipc(
  title: String,
  body: Option<String>,
  icon: Option<String>,
  tag: Option<String>,
) {
  let Some(ctx) = v8_context_get_current_context() else { return };
  let Some(mut frame) = ctx.frame() else { return };
  let url = CefString::from(&frame.url()).to_string();
  let origin = origin_from(&url);

  let name = CefString::from(IPC_NOTIFICATION_SHOW);
  let Some(mut msg) = process_message_create(Some(&name)) else { return };
  let Some(mut args) = msg.argument_list() else { return };
  args.set_size(6);
  args.set_string(0, Some(&CefString::from(title.as_str())));
  args.set_string(1, Some(&CefString::from(body.as_deref().unwrap_or(""))));
  args.set_string(2, Some(&CefString::from(icon.as_deref().unwrap_or(""))));
  args.set_string(3, Some(&CefString::from(tag.as_deref().unwrap_or(""))));
  args.set_string(4, Some(&CefString::from(url.as_str())));
  args.set_string(5, Some(&CefString::from(origin.as_str())));

  frame.send_process_message(
    cef::sys::cef_process_id_t::PID_BROWSER.into(),
    Some(&mut msg),
  );
}

fn send_permission_request_ipc() {
  let Some(ctx) = v8_context_get_current_context() else { return };
  let Some(mut frame) = ctx.frame() else { return };
  let name = CefString::from(IPC_PERMISSION_REQUEST);
  let Some(mut msg) = process_message_create(Some(&name)) else { return };
  frame.send_process_message(
    cef::sys::cef_process_id_t::PID_BROWSER.into(),
    Some(&mut msg),
  );
}

wrap_v8_handler! {
  struct NotificationCtorHandler;

  impl V8Handler {
    fn execute(
      &self,
      _name: Option<&CefString>,
      _object: Option<&mut V8Value>,
      arguments: Option<&[Option<V8Value>]>,
      retval: Option<&mut Option<V8Value>>,
      _exception: Option<&mut CefString>,
    ) -> ::std::os::raw::c_int {
      let (mut title, mut body, mut icon, mut tag) = (String::new(), None, None, None);

      if let Some(args) = arguments {
        if let Some(Some(arg0)) = args.first() {
          if arg0.is_string() != 0 {
            title = CefString::from(&arg0.string_value()).to_string();
          }
        }
        if let Some(Some(arg1)) = args.get(1) {
          if arg1.is_object() != 0 {
            body = read_string_prop(arg1, "body");
            icon = read_string_prop(arg1, "icon");
            tag = read_string_prop(arg1, "tag");
          }
        }
      }

      send_notification_ipc(title, body, icon, tag);

      if let Some(retval) = retval {
        *retval = v8_value_create_undefined();
      }
      1
    }
  }
}

wrap_v8_handler! {
  struct RequestPermissionHandler;

  impl V8Handler {
    fn execute(
      &self,
      _name: Option<&CefString>,
      _object: Option<&mut V8Value>,
      _arguments: Option<&[Option<V8Value>]>,
      retval: Option<&mut Option<V8Value>>,
      _exception: Option<&mut CefString>,
    ) -> ::std::os::raw::c_int {
      send_permission_request_ipc();

      if let Some(mut promise) = v8_value_create_promise() {
        let granted = CefString::from("granted");
        let mut value = v8_value_create_string(Some(&granted));
        promise.resolve_promise(value.as_mut());
        if let Some(retval) = retval {
          *retval = Some(promise);
        }
      } else if let Some(retval) = retval {
        let granted = CefString::from("granted");
        *retval = v8_value_create_string(Some(&granted));
      }
      1
    }
  }
}

fn ro_attrs() -> cef_v8_propertyattribute_t {
  cef_v8_propertyattribute_t(
    cef_v8_propertyattribute_t::V8_PROPERTY_ATTRIBUTE_READONLY.0
      | cef_v8_propertyattribute_t::V8_PROPERTY_ATTRIBUTE_DONTDELETE.0,
  )
}

fn install_notification_hook(context: &mut V8Context) {
  let Some(window) = context.global() else { return };

  let notif_name = CefString::from("Notification");
  let mut handler = NotificationCtorHandler::new();
  let Some(mut notif_fn) = v8_value_create_function(Some(&notif_name), Some(&mut handler)) else {
    return;
  };

  let permission_key = CefString::from("permission");
  let granted = CefString::from("granted");
  let mut granted_val = v8_value_create_string(Some(&granted));
  notif_fn.set_value_bykey(
    Some(&permission_key),
    granted_val.as_mut(),
    ro_attrs().into(),
  );

  let req_name = CefString::from("requestPermission");
  let mut req_handler = RequestPermissionHandler::new();
  let mut req_fn = v8_value_create_function(Some(&req_name), Some(&mut req_handler));
  notif_fn.set_value_bykey(Some(&req_name), req_fn.as_mut(), ro_attrs().into());

  window.set_value_bykey(Some(&notif_name), Some(&mut notif_fn), ro_attrs().into());
}

wrap_render_process_handler! {
  struct OpenHumanRenderProcessHandler;

  impl RenderProcessHandler {
    fn on_context_created(
      &self,
      _browser: Option<&mut Browser>,
      _frame: Option<&mut Frame>,
      context: Option<&mut V8Context>,
    ) {
      if let Some(context) = context {
        install_notification_hook(context);
      }
    }
  }
}

wrap_app! {
  struct RenderApp;

  impl App {
    fn render_process_handler(&self) -> Option<RenderProcessHandler> {
      Some(OpenHumanRenderProcessHandler::new())
    }
  }
}

fn main() {
  let args = Args::new();

  #[cfg(all(target_os = "macos", feature = "sandbox"))]
  let _sandbox = {
    let mut sandbox = cef::sandbox::Sandbox::new();
    sandbox.initialize(args.as_main_args());
    sandbox
  };

  #[cfg(target_os = "macos")]
  let _loader = {
    let loader = library_loader::LibraryLoader::new(&std::env::current_exe().unwrap(), true);
    assert!(loader.load());
    loader
  };

  let mut app = RenderApp::new();
  execute_process(
    Some(args.as_main_args()),
    Some(&mut app),
    std::ptr::null_mut(),
  );
}
