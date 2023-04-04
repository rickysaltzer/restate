use bytes::Bytes;
use bytestring::ByteString;
use opentelemetry_api::trace::{SpanContext, TraceContextExt};
use std::fmt;
use std::fmt::{Display, Formatter};
use tracing::{info_span, Span};
use tracing_opentelemetry::OpenTelemetrySpanExt;
use uuid::Uuid;

/// Identifying a member of a raft group
pub type PeerId = u64;

/// Identifying the leader epoch of a raft group leader
pub type LeaderEpoch = u64;

/// Identifying the partition
pub type PartitionId = u64;

/// The leader epoch of a given partition
pub type PartitionLeaderEpoch = (PartitionId, LeaderEpoch);

pub type EntryIndex = u32;

/// Identifying to which partition a key belongs. This is unlike the [`PartitionId`]
/// which identifies a consecutive range of partition keys.
pub type PartitionKey = u64;

/// Discriminator for invocation instances
pub type InvocationId = Uuid;

/// Id of a single service invocation.
///
/// A service invocation id is composed of a [`ServiceId`] and an [`InvocationId`]
/// that makes the id unique.
#[derive(Eq, Hash, PartialEq, Clone, Debug)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct ServiceInvocationId {
    /// Identifies the invoked service
    pub service_id: ServiceId,
    /// Uniquely identifies this invocation instance
    pub invocation_id: InvocationId,
}

impl Display for ServiceInvocationId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}[{:?}]({})",
            self.service_id.service_name, self.service_id.key, self.invocation_id
        )
    }
}

impl ServiceInvocationId {
    pub fn new(
        service_name: impl Into<ByteString>,
        key: impl Into<Bytes>,
        invocation_id: impl Into<InvocationId>,
    ) -> Self {
        Self {
            service_id: ServiceId {
                service_name: service_name.into(),
                key: key.into(),
            },
            invocation_id: invocation_id.into(),
        }
    }
}

/// Id of a keyed service instance.
///
/// Services are isolated by key. This means that there cannot be two concurrent
/// invocations for the same service instance (service name, key).
#[derive(Eq, Hash, PartialEq, PartialOrd, Ord, Clone, Debug)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct ServiceId {
    /// Identifies the grpc service
    pub service_name: ByteString,
    /// Identifies the service instance for the given service name
    pub key: Bytes,
}

impl ServiceId {
    pub fn new(service_name: impl Into<ByteString>, key: impl Into<Bytes>) -> Self {
        Self {
            service_name: service_name.into(),
            key: key.into(),
        }
    }
}

/// Representing a service invocation
#[derive(Debug, Clone)]
pub struct ServiceInvocation {
    pub id: ServiceInvocationId,
    pub method_name: ByteString,
    pub argument: Bytes,
    pub response_sink: ServiceInvocationResponseSink,
    pub span_context: ServiceInvocationSpanContext,
}

impl ServiceInvocation {
    /// Create a new [`ServiceInvocation`].
    ///
    /// This method returns the [`Span`] associated to the created [`ServiceInvocation`].
    /// It is not required to keep this [`Span`] around for the whole lifecycle of the invocation.
    /// On the contrary, it is encouraged to drop it as soon as possible,
    /// to let the exporter commit this span to jaeger/zipkin to visualize intermediate results of the invocation.
    pub fn new(
        id: ServiceInvocationId,
        method_name: ByteString,
        argument: Bytes,
        response_sink: ServiceInvocationResponseSink,
        related_span: SpanRelation,
    ) -> (Self, Span) {
        let (span_context, span) = ServiceInvocationSpanContext::start(
            &id.service_id.service_name,
            &method_name,
            &id.service_id.key,
            id.invocation_id,
            related_span,
        );
        (
            Self {
                id,
                method_name,
                argument,
                response_sink,
                span_context,
            },
            span,
        )
    }
}

/// Representing a response for a caller
#[derive(Debug, Clone)]
pub struct InvocationResponse {
    pub id: ServiceInvocationId,
    pub entry_index: EntryIndex,
    pub result: ResponseResult,
}

#[derive(Debug, Clone)]
pub enum ResponseResult {
    Success(Bytes),
    Failure(i32, ByteString),
}

#[derive(Debug, PartialEq, Eq, Clone, Copy, Hash)]
pub struct IngressId(pub std::net::SocketAddr);

/// Definition of the sink where to send the result of a service invocation.
#[derive(Debug, PartialEq, Eq, Clone)]
pub enum ServiceInvocationResponseSink {
    /// The invocation has been created by a partition processor and is expecting a response.
    PartitionProcessor {
        caller: ServiceInvocationId,
        entry_index: EntryIndex,
    },
    /// The invocation has been generated by a request received at an ingress, and the client is expecting a response back.
    Ingress(IngressId),
    /// The invocation is fire and forget, response can be discarded.
    None,
}

/// This struct contains the relevant span information for a [`ServiceInvocation`].
/// It can be used to create related spans, such as child spans,
/// using [`ServiceInvocationSpanContext::as_cause`] or [`ServiceInvocationSpanContext::as_parent`].
#[derive(Debug, PartialEq, Eq, Clone)]
pub struct ServiceInvocationSpanContext(SpanContext);

impl ServiceInvocationSpanContext {
    /// See [`ServiceInvocation::new`] for more details.
    pub fn start(
        service_name: &str,
        method_name: &str,
        service_key: impl fmt::Debug,
        invocation_id: impl Display,
        related_span: SpanRelation,
    ) -> (ServiceInvocationSpanContext, Span) {
        // Create the span
        let span = info_span!(
            "service_invocation",
            rpc.system = "restate",
            rpc.service = service_name,
            rpc.method = method_name,
            restate.invocation.key = ?service_key,
            restate.invocation.id = %invocation_id);

        // Attach the related span.
        // Note: As it stands with tracing_opentelemetry 0.18 there seems to be
        // an ordering relationship between using OpenTelemetrySpanExt::context() and
        // OpenTelemetrySpanExt::set_parent().
        // If we invert the order, the spans won't link correctly because they'll have a different Trace ID.
        // This is the reason why this method gets a SpanRelation, rather than letting the caller
        // link the spans.
        // https://github.com/tokio-rs/tracing/issues/2520
        related_span.attach_to_span(&span);

        // Retrieve the OTEL SpanContext we want to propagate
        let span_context = span.context().span().span_context().clone();

        (ServiceInvocationSpanContext(span_context), span)
    }

    pub fn as_cause(&self) -> SpanRelation {
        SpanRelation::CausedBy(self.0.clone())
    }

    pub fn as_parent(&self) -> SpanRelation {
        SpanRelation::Parent(self.0.clone())
    }
}

impl From<ServiceInvocationSpanContext> for SpanContext {
    fn from(value: ServiceInvocationSpanContext) -> Self {
        value.0
    }
}

/// Span relation, used to propagate tracing contexts.
#[derive(Debug, PartialEq, Eq, Clone)]
pub enum SpanRelation {
    None,
    Parent(SpanContext),
    CausedBy(SpanContext),
}

impl SpanRelation {
    /// Attach this [`SpanRelation`] to the given [`Span`]
    pub fn attach_to_span(self, span: &Span) {
        match self {
            SpanRelation::Parent(parent) => {
                span.set_parent(opentelemetry_api::Context::new().with_remote_span_context(parent))
            }
            SpanRelation::CausedBy(cause) => span.add_link(cause),
            _ => {}
        };
    }
}

/// Wrapper that extends a message with its target peer to which the message should be sent.
pub type PeerTarget<Msg> = (PeerId, Msg);

/// Index type used messages in the runtime
pub type MessageIndex = u64;

#[derive(Debug, Clone, Copy)]
pub enum AckKind {
    Acknowledge(MessageIndex),
    Duplicate(MessageIndex),
}

/// Milliseconds since the unix epoch
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MillisSinceEpoch(u64);

impl MillisSinceEpoch {
    pub const UNIX_EPOCH: MillisSinceEpoch = MillisSinceEpoch::new(0);
    pub const MAX: MillisSinceEpoch = MillisSinceEpoch::new(u64::MAX);

    pub const fn new(millis_since_epoch: u64) -> Self {
        MillisSinceEpoch(millis_since_epoch)
    }

    pub fn as_u64(&self) -> u64 {
        self.0
    }
}

impl From<u64> for MillisSinceEpoch {
    fn from(value: u64) -> Self {
        Self::new(value)
    }
}

impl Display for MillisSinceEpoch {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "{} ms since epoch", self.0)
    }
}

/// Entry of the inbox
#[derive(Debug, Clone)]
pub struct InboxEntry {
    pub inbox_sequence_number: MessageIndex,
    pub service_invocation: ServiceInvocation,
}

impl InboxEntry {
    pub fn new(inbox_sequence_number: MessageIndex, service_invocation: ServiceInvocation) -> Self {
        Self {
            inbox_sequence_number,
            service_invocation,
        }
    }
}

/// State machine representation of the [`ServiceInvocationResponseSink`]
#[derive(Debug, Clone)]
pub enum ResponseSink {
    Ingress(IngressId, ServiceInvocationId),
    PartitionProcessor(ServiceInvocationId, EntryIndex),
}

impl ResponseSink {
    pub fn from_service_invocation_response_sink(
        service_invocation_id: &ServiceInvocationId,
        response_sink: &ServiceInvocationResponseSink,
    ) -> Option<ResponseSink> {
        match response_sink {
            ServiceInvocationResponseSink::Ingress(ingress_id) => Some(ResponseSink::Ingress(
                *ingress_id,
                service_invocation_id.clone(),
            )),
            ServiceInvocationResponseSink::None => None,
            ServiceInvocationResponseSink::PartitionProcessor {
                entry_index,
                caller,
            } => Some(ResponseSink::PartitionProcessor(
                caller.clone(),
                *entry_index,
            )),
        }
    }
}
