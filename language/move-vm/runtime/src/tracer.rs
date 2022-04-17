use crate::loader::Function;
use move_core_types::gas_schedule::{GasAlgebra, GasCarrier};
use move_vm_types::gas_schedule::GasStatus;
use std::fmt::Write;
#[derive(Default)]
pub(crate) struct VMTracer {
    tracing: Vec<String>,
    trace_data: Vec<String>,
    last_remaining_gas: Option<GasCarrier>,
}

impl VMTracer {
    fn gas_used_since_last_event(&mut self, remaining_gas: GasCarrier) -> GasCarrier {
        let l = self.last_remaining_gas.unwrap_or(remaining_gas);
        self.last_remaining_gas = Some(remaining_gas);
        l - remaining_gas
    }

    #[allow(unused)]
    pub fn get_trace(&self) -> String {
        self.trace_data.join("\n")
    }
}

impl Tracer for VMTracer {
    fn trace_function_call_start(&mut self, function: &Function, gas_status: &GasStatus) {
        let gas_used = self.gas_used_since_last_event(gas_status.remaining_gas().get());
        if !self.tracing.is_empty() {
            let mut data = String::new();
            let mut call_stack = self.tracing.iter();
            if let Some(root) = call_stack.next() {
                write!(&mut data, "{}", root).expect("expected: write to String never fails");
            }
            for call in call_stack {
                write!(&mut data, "; {}", call).expect("expected: write to String never fails");
            }
            write!(&mut data, " {}", gas_used).expect("expected: write to String never fails");
            self.trace_data.push(data);
        }

        self.tracing.push(function.pretty_string());
    }

    fn trace_function_call_end(&mut self, _function: &Function, gas_status: &GasStatus) {
        let gas_used = self.gas_used_since_last_event(gas_status.remaining_gas().get());
        {
            let mut data = String::new();
            let mut call_stack = self.tracing.iter().take(self.tracing.len());
            if let Some(root) = call_stack.next() {
                write!(&mut data, "{}", root).expect("expected: write to String never fails");
            }
            for call in call_stack {
                write!(&mut data, "; {}", call).expect("expected: write to String never fails");
            }
            write!(&mut data, " {}", gas_used).expect("expected: write to String never fails");
            self.trace_data.push(data);
        }

        self.tracing.pop().unwrap();
    }
}
pub(crate) trait Tracer {
    fn trace_function_call_start(&mut self, function: &Function, gas_status: &GasStatus);
    fn trace_function_call_end(&mut self, function: &Function, gas_status: &GasStatus);
}
