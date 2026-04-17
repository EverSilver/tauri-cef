// Native V8-level interceptor for the Web Notification API.
//
// Runs inside the CEF render process (before any page script executes via
// `on_context_created`). Replaces `window.Notification` with a native V8 function
// that captures `new Notification(title, options)` calls and forwards them to the
// browser process over CEF's inter-process `ProcessMessage` channel.
//
// Also forces `Notification.permission === "granted"` and makes
// `Notification.requestPermission()` resolve synchronously to `"granted"` so
// pages never see a permission prompt.

use cef::{rc::*, *};
use cef::sys::{cef_process_id_t::PID_BROWSER, cef_v8_propertyattribute_t};

fn origin_from_url(url: &str) -> String {
  match url::Url::parse(url) {
    Ok(u) => match u.host_str() {
      Some(host) => match u.port() {
        Some(port) => format!("{}://{}:{}", u.scheme(), host, port),
        None => format!("{}://{}", u.scheme(), host),
      },
      None => String::new(),
    },
    Err(_) => String::new(),
  }
}

fn attr_none() -> V8Propertyattribute {
  V8Propertyattribute::from(cef_v8_propertyattribute_t(0))
}

fn read_string_key(object: &V8Value, key: &str) -> Option<String> {
  let cef_key = CefString::from(key);
  if object.has_value_bykey(Some(&cef_key)) != 1 {
    return None;
  }
  let value = object.value_bykey(Some(&cef_key))?;
  if value.is_string() != 1 {
    return None;
  }
  let s = CefString::from(&value.string_value()).to_string();
  if s.is_empty() { None } else { Some(s) }
}

fn current_frame_info() -> (Option<Frame>, String, String) {
  let Some(ctx) = cef::v8_context_get_current_context() else {
    return (None, String::new(), String::new());
  };
  let Some(frame) = ctx.frame() else {
    return (None, String::new(), String::new());
  };
  let url = CefString::from(&frame.url()).to_string();
  let origin = origin_from_url(&url);
  (Some(frame), url, origin)
}

fn send_show_message(
  title: &str,
  body: Option<&str>,
  icon: Option<&str>,
  tag: Option<&str>,
  frame_url: &str,
  origin: &str,
  frame: &mut Frame,
) {
  let Some(mut msg) = process_message_create(Some(&CefString::from("openhuman.notification.show")))
  else {
    return;
  };
  let Some(args) = msg.argument_list() else {
    return;
  };
  args.set_size(6);
  args.set_string(0, Some(&CefString::from(title)));
  args.set_string(1, Some(&CefString::from(body.unwrap_or(""))));
  args.set_string(2, Some(&CefString::from(icon.unwrap_or(""))));
  args.set_string(3, Some(&CefString::from(tag.unwrap_or(""))));
  args.set_string(4, Some(&CefString::from(frame_url)));
  args.set_string(5, Some(&CefString::from(origin)));
  frame.send_process_message(ProcessId::from(PID_BROWSER), Some(&mut msg));
}

// Handler for the wrapped `Notification` constructor.
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
      let args = arguments.unwrap_or(&[]);
      let title = args
        .first()
        .and_then(|a| a.as_ref())
        .filter(|a| a.is_string() == 1)
        .map(|a| CefString::from(&a.string_value()).to_string())
        .unwrap_or_default();

      let (mut body, mut icon, mut tag) = (None, None, None);
      if let Some(opts) = args.get(1).and_then(|a| a.as_ref())
        && opts.is_object() == 1
      {
        body = read_string_key(opts, "body");
        icon = read_string_key(opts, "icon");
        tag = read_string_key(opts, "tag");
      }

      let (frame, frame_url, origin) = current_frame_info();
      if let Some(mut frame) = frame {
        send_show_message(
          &title,
          body.as_deref(),
          icon.as_deref(),
          tag.as_deref(),
          &frame_url,
          &origin,
          &mut frame,
        );
      }

      // Return a minimal object so `new Notification(...)` yields something
      // well-formed. Only `title` is exposed; methods like `close()` are no-ops
      // because the page never actually sees a real platform notification.
      if let Some(retval) = retval
        && let Some(obj) = v8_value_create_object(None, None)
      {
        if let Some(mut title_val) = v8_value_create_string(Some(&CefString::from(title.as_str()))) {
          let _ = obj.set_value_bykey(
            Some(&CefString::from("title")),
            Some(&mut title_val),
            attr_none(),
          );
        }
        *retval = Some(obj);
      }
      1
    }
  }
}

// Handler for `Notification.requestPermission()`.
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
      // Emit a telemetry message so the browser side sees the request.
      let (frame, frame_url, origin) = current_frame_info();
      if let Some(mut frame) = frame
        && let Some(mut msg) = process_message_create(Some(&CefString::from(
          "openhuman.notification.permission_request",
        )))
      {
        if let Some(args) = msg.argument_list() {
          args.set_size(2);
          args.set_string(0, Some(&CefString::from(frame_url.as_str())));
          args.set_string(1, Some(&CefString::from(origin.as_str())));
        }
        frame.send_process_message(ProcessId::from(PID_BROWSER), Some(&mut msg));
      }

      // Resolve a promise with `"granted"` — no prompt shown.
      if let Some(retval) = retval
        && let Some(promise) = v8_value_create_promise()
      {
        if let Some(mut granted) = v8_value_create_string(Some(&CefString::from("granted"))) {
          let _ = promise.resolve_promise(Some(&mut granted));
        }
        *retval = Some(promise);
      }
      1
    }
  }
}

fn install_notification_hook(context: &mut V8Context) {
  let Some(global) = context.global() else { return; };

  let name = CefString::from("Notification");
  let mut ctor_handler: cef::V8Handler = NotificationCtorHandler::new();
  let Some(mut ctor_fn) = v8_value_create_function(Some(&name), Some(&mut ctor_handler)) else {
    return;
  };

  // `Notification.permission` — always "granted".
  if let Some(mut granted) = v8_value_create_string(Some(&CefString::from("granted"))) {
    let _ = ctor_fn.set_value_bykey(
      Some(&CefString::from("permission")),
      Some(&mut granted),
      attr_none(),
    );
  }

  // `Notification.requestPermission` — native function returning a resolved promise.
  let request_name = CefString::from("requestPermission");
  let mut req_handler: cef::V8Handler = RequestPermissionHandler::new();
  if let Some(mut req_fn) = v8_value_create_function(Some(&request_name), Some(&mut req_handler)) {
    let _ = ctor_fn.set_value_bykey(
      Some(&CefString::from("requestPermission")),
      Some(&mut req_fn),
      attr_none(),
    );
  }

  // Replace the global `Notification`.
  let _ = global.set_value_bykey(Some(&name), Some(&mut ctor_fn), attr_none());
}

wrap_render_process_handler! {
  pub(crate) struct OpenHumanRenderProcessHandler;

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

/// Minimal [`App`] that only provides the render-process handler.
///
/// Used from [`crate::run_cef_helper_process`] so subprocess entry points install the
/// same V8 notification hook that browser-side code installs via [`crate::cef_impl::TauriApp`].
wrap_app! {
  pub(crate) struct RenderApp;

  impl App {
    fn render_process_handler(&self) -> Option<RenderProcessHandler> {
      Some(OpenHumanRenderProcessHandler::new())
    }
  }
}
