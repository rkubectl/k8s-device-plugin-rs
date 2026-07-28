use std::fmt;
use std::sync::Arc;
use std::sync::Mutex;

use k8s_device_plugin_test::dra_registration::MockRegistrationClient;
use tempfile::TempDir;
use tracing::Level;
use tracing::field::Field;
use tracing::field::Visit;
use tracing_subscriber::Layer;
use tracing_subscriber::layer::Context;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::registry::LookupSpan;

use super::*;

fn make_server() -> (DraRegistrationServer, TempDir) {
    let dir = TempDir::new().expect("create temp dir for registration socket");
    let socket_path = dir.path().join("plugin-reg.sock");
    let server = DraRegistrationServer::for_test(
        "example.com/widget",
        "/var/lib/kubelet/plugins/example.com/widget/plugin.sock",
        socket_path,
    );
    (server, dir)
}

#[tokio::test]
async fn get_info_returns_configured_plugin_info() {
    let (server, _dir) = make_server();
    let socket_path = server.socket_path().to_path_buf();
    let handle = server.spawn().await.expect("spawn registration server");

    let mut client = MockRegistrationClient::connect(&socket_path)
        .await
        .expect("connect to registration socket");
    let info = client.get_info().await.expect("get_info call");

    assert_eq!(info.r#type, "DRAPlugin");
    assert_eq!(info.name, "example.com/widget");
    assert_eq!(
        info.endpoint,
        "/var/lib/kubelet/plugins/example.com/widget/plugin.sock"
    );
    assert_eq!(info.supported_versions, vec!["v1".to_string()]);

    handle.abort();
}

#[tokio::test]
async fn notify_registration_status_success_does_not_error() {
    let (server, _dir) = make_server();
    let socket_path = server.socket_path().to_path_buf();
    let handle = server.spawn().await.expect("spawn registration server");

    let mut client = MockRegistrationClient::connect(&socket_path)
        .await
        .expect("connect to registration socket");
    client
        .notify_registration_status(true, "")
        .await
        .expect("notify call should not error on success");

    handle.abort();
}

/// Records every tracing event's level and formatted fields, so a test can
/// assert on what got logged without a real subscriber's output.
#[derive(Clone, Default)]
struct CapturedEvents(Arc<Mutex<Vec<(Level, String)>>>);

impl CapturedEvents {
    fn events(&self) -> Vec<(Level, String)> {
        self.0.lock().expect("captured-events lock").clone()
    }
}

struct FieldsVisitor(String);

impl Visit for FieldsVisitor {
    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        self.0.push_str(&format!(" {}={value:?}", field.name()));
    }
}

impl<S> Layer<S> for CapturedEvents
where
    S: tracing::Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_event(&self, event: &tracing::Event<'_>, _ctx: Context<'_, S>) {
        let mut visitor = FieldsVisitor(String::new());
        event.record(&mut visitor);
        self.0
            .lock()
            .expect("captured-events lock")
            .push((*event.metadata().level(), visitor.0));
    }
}

#[tokio::test]
async fn notify_registration_status_failure_logs_error() {
    let captured = CapturedEvents::default();
    let subscriber = tracing_subscriber::registry().with(captured.clone());
    let _guard = tracing::subscriber::set_default(subscriber);

    let (server, _dir) = make_server();
    let socket_path = server.socket_path().to_path_buf();
    let handle = server.spawn().await.expect("spawn registration server");

    let mut client = MockRegistrationClient::connect(&socket_path)
        .await
        .expect("connect to registration socket");
    client
        .notify_registration_status(false, "boom")
        .await
        .expect("notify call should not error even when plugin_registered is false");

    let events = captured.events();
    assert!(
        events
            .iter()
            .any(|(level, msg)| *level == Level::ERROR && msg.contains("boom")),
        "expected an ERROR event mentioning \"boom\", got: {events:?}"
    );

    handle.abort();
}
