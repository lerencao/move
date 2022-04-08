use move_core_types::account_address::AccountAddress;
use move_core_types::gas_schedule::GasCarrier;
use std::fmt;
use std::fmt::{Debug, Write};
use std::fs::File;
use std::io::BufWriter;
use std::marker::PhantomData;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};
use tracing::field::Field;
use tracing::span::Attributes;
use tracing::{Event, Id, Metadata, Subscriber};
use tracing_subscriber::field::Visit;
use tracing_subscriber::layer::Context;
use tracing_subscriber::registry::{LookupSpan, SpanRef};
use tracing_subscriber::Layer;

#[derive(Debug)]
pub struct GasLayer<S, W> {
    out: Arc<Mutex<W>>,
    last_remaining_gas: RwLock<GasCarrier>,
    _inner: PhantomData<S>,
}

#[derive(Debug, Clone)]
struct SpanCallInfo {
    module_address: AccountAddress,
    module_name: String,
    function_name: String,
}
impl Default for SpanCallInfo {
    fn default() -> Self {
        Self {
            module_address: AccountAddress::ZERO,
            module_name: String::default(),
            function_name: String::default(),
        }
    }
}

impl From<SpanAttributesVisitor> for SpanCallInfo {
    fn from(v: SpanAttributesVisitor) -> Self {
        v.inner
    }
}

#[derive(Default)]
struct SpanAttributesVisitor {
    inner: SpanCallInfo,
}

impl Visit for SpanAttributesVisitor {
    fn record_str(&mut self, field: &Field, value: &str) {
        match field.name() {
            "module_address" => {
                if let Ok(a) = AccountAddress::from_hex_literal(value) {
                    self.inner.module_address = a;
                }
            }
            "module_name" => {
                self.inner.module_name = value.to_string();
            }
            "function_name" => {
                self.inner.function_name = value.to_string();
            }
            _ => {}
        }
    }

    fn record_debug(&mut self, _field: &Field, _value: &dyn Debug) {}
}

#[derive(Default, Debug, Clone, Copy)]
struct GasEvent {
    remaining_gas: GasCarrier,
}
impl From<GasEventVisitor> for GasEvent {
    fn from(v: GasEventVisitor) -> Self {
        v.inner
    }
}
#[derive(Default, Debug, Clone, Copy)]
struct GasEventVisitor {
    inner: GasEvent,
}
impl Visit for GasEventVisitor {
    fn record_u64(&mut self, field: &Field, value: u64) {
        if field.name() == "remaining_gas" {
            self.inner.remaining_gas = value;
        }
    }
    fn record_debug(&mut self, _field: &Field, _value: &dyn Debug) {}
}

impl<S, W> GasLayer<S, W>
where
    //S: Subscriber + for<'span> LookupSpan<'span>,
    W: std::io::Write + 'static,
{
    /// Returns a new `GasLayer` that outputs all folded stack samples to the
    /// provided writer.
    pub fn new(writer: W, remaining_gas: GasCarrier) -> Self {
        Self {
            out: Arc::new(Mutex::new(writer)),
            last_remaining_gas: RwLock::new(remaining_gas),
            _inner: PhantomData,
        }
    }

    /// Returns a `FlushGuard` which will flush the `FlameLayer`'s writer when
    /// it is dropped, or when `flush` is manually invoked on the guard.
    pub fn flush_on_drop(&self) -> FlushGuard<W> {
        FlushGuard {
            out: self.out.clone(),
        }
    }
}

impl<S, W> Layer<S> for GasLayer<S, W>
where
    S: Subscriber + for<'span> LookupSpan<'span>,
    W: std::io::Write + 'static,
{
    fn enabled(&self, metadata: &Metadata<'_>, _ctx: Context<'_, S>) -> bool {
        // TODO: filter by metadata's target
        if metadata.is_event() {
            metadata.target() == "start" || metadata.target() == "end"
        } else if metadata.is_span() {
            metadata.name() == "root"
                || metadata.name() == "call"
                || metadata.name() == "call_generic"
        } else {
            false
        }
    }
    fn on_new_span(&self, attrs: &Attributes<'_>, id: &Id, ctx: Context<'_, S>) {
        let mut visitor = SpanAttributesVisitor::default();
        attrs.values().record(&mut visitor);
        let span_info: SpanCallInfo = visitor.into();
        ctx.span(id).unwrap().extensions_mut().insert(span_info);
    }

    fn on_event(&self, _event: &Event<'_>, ctx: Context<'_, S>) {
        let event_name = _event.metadata().target();
        let span_id = _event.parent().unwrap();
        if event_name == "start" {
            let gas_event: GasEvent = {
                let mut visitor = GasEventVisitor::default();
                _event.record(&mut visitor);
                visitor.into()
            };
            let gas_used = self.gas_used_since_last_event(gas_event.remaining_gas);

            let first = ctx
                .span(span_id)
                .expect("expected: span id exists in registry");

            if first.parent().is_none() {
                return;
            }

            let mut stack = String::new();

            if let Some(second) = first.parent() {
                let mut call_stack = second.scope().from_root();
                if let Some(root) = call_stack.next() {
                    write(&mut stack, root).expect("expected: write to String never fails");
                }
                for parent in call_stack {
                    stack += "; ";
                    write(&mut stack, parent).expect("expected: write to String never fails");
                }
            }
            stack += &format!(" {}", gas_used);
            // write!(&mut stack, " {}", gas_used).expect("expected: write to String never fails");
            let _ = writeln!(*self.out.lock().unwrap(), "{}", stack);
        } else if event_name == "end" {
            let gas_event: GasEvent = {
                let mut visitor = GasEventVisitor::default();
                _event.record(&mut visitor);
                visitor.into()
            };
            let gas_used = self.gas_used_since_last_event(gas_event.remaining_gas);

            let first = ctx
                .span(span_id)
                .expect("expected: span id exists in registry");

            let mut stack = String::new();

            {
                let mut call_stack = first.scope().from_root();
                if let Some(root) = call_stack.next() {
                    write(&mut stack, root).expect("expected: write to String never fails");
                }
                for parent in call_stack {
                    stack += "; ";
                    write(&mut stack, parent).expect("expected: write to String never fails");
                }
            }

            stack += &format!(" {}", gas_used);
            let _ = writeln!(*self.out.lock().unwrap(), "{}", stack);
        }
    }
}

impl<S, W> GasLayer<S, W>
where
    S: Subscriber + for<'span> LookupSpan<'span>,
    W: std::io::Write + 'static,
{
    fn gas_used_since_last_event(&self, remaining_gas: GasCarrier) -> GasCarrier {
        let gas_used = *self.last_remaining_gas.read().unwrap() - remaining_gas;
        *self.last_remaining_gas.write().unwrap() = remaining_gas;
        gas_used
    }
}
impl<S> GasLayer<S, BufWriter<File>>
where
    S: Subscriber + for<'span> LookupSpan<'span>,
{
    /// Constructs a `FlameLayer` that outputs to a `BufWriter` to the given path, and a
    /// `FlushGuard` to ensure the writer is flushed.
    pub fn with_file(
        path: impl AsRef<Path>,
        remaining_gas: GasCarrier,
    ) -> Result<(Self, FlushGuard<BufWriter<File>>), Error> {
        let path = path.as_ref();
        let file = File::create(path)
            .map_err(|source| Kind::CreateFile {
                path: path.into(),
                source,
            })
            .map_err(Error)?;
        let writer = BufWriter::new(file);
        let layer = Self::new(writer, remaining_gas);
        let guard = layer.flush_on_drop();
        Ok((layer, guard))
    }
}

fn write<S>(dest: &mut String, _span: SpanRef<'_, S>) -> fmt::Result
where
    S: Subscriber + for<'span> LookupSpan<'span>,
{
    let exts = _span.extensions();
    let call_info = exts.get::<SpanCallInfo>().unwrap();
    if !call_info.module_name.is_empty() {
        write!(
            dest,
            "{}::{}::",
            call_info.module_address, call_info.module_name
        )?;
    }

    write!(dest, "{}", call_info.function_name)?;

    // if config.file_and_line {
    //     if let Some(file) = span.metadata().file() {
    //         write!(dest, ":{}", file)?;
    //     }
    //
    //     if let Some(line) = span.metadata().line() {
    //         write!(dest, ":{}", line)?;
    //     }
    // }

    Ok(())
}

/// An RAII guard for managing flushing a global writer that is
/// otherwise inaccessible.
///
/// This type is only needed when using
/// `tracing::subscriber::set_global_default`, which prevents the drop
/// implementation of layers from running when the program exits.
#[must_use]
#[derive(Debug)]
pub struct FlushGuard<W>
where
    W: std::io::Write + 'static,
{
    out: Arc<Mutex<W>>,
}

impl<W> FlushGuard<W>
where
    W: std::io::Write + 'static,
{
    /// Flush the internal writer of the `FlameLayer`, ensuring that all
    /// intermediately buffered contents reach their destination.
    pub fn flush(&self) -> Result<(), Error> {
        let mut guard = match self.out.lock() {
            Ok(guard) => guard,
            Err(e) => {
                if !std::thread::panicking() {
                    panic!("{}", e);
                } else {
                    return Ok(());
                }
            }
        };

        guard.flush().map_err(Kind::FlushFile).map_err(Error)
    }
}

impl<W> Drop for FlushGuard<W>
where
    W: std::io::Write + 'static,
{
    fn drop(&mut self) {
        match self.flush() {
            Ok(_) => (),
            Err(e) => e.report(),
        }
    }
}

/// The error type for `tracing-flame`
#[derive(Debug)]
pub struct Error(pub(crate) Kind);

impl Error {
    pub(crate) fn report(&self) {
        let current_error: &dyn std::error::Error = self;
        let mut current_error = Some(current_error);
        let mut ind = 0;

        eprintln!("Error:");

        while let Some(error) = current_error {
            eprintln!("    {}: {}", ind, error);
            ind += 1;
            current_error = error.source();
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, f)
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match &self.0 {
            Kind::CreateFile { ref source, .. } => Some(source),
            Kind::FlushFile(ref source) => Some(source),
        }
    }
}

#[derive(Debug)]
pub(crate) enum Kind {
    CreateFile {
        source: std::io::Error,
        path: PathBuf,
    },
    FlushFile(std::io::Error),
}

impl fmt::Display for Kind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::CreateFile { path, .. } => {
                write!(f, "cannot create output file. path={}", path.display())
            }
            Self::FlushFile { .. } => write!(f, "cannot flush output buffer"),
        }
    }
}
