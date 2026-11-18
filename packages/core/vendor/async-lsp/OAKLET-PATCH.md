# Oaklet patch to async-lsp 0.2.4

Upstream: https://github.com/oxalica/async-lsp (crates.io 0.2.4).
Original source and licenses retained; unused example/test manifest entries removed.

Request IDs are allocated by the shared socket. Dropping an unfinished outgoing
request future enqueues CancelOutgoing in the same FIFO as the request. The loop
removes the pending response sender and emits $/cancelRequest only if still pending.
Completed requests do not cancel; late responses are ignored. No public API change.
This repairs cancellation without replacing JSON-RPC or closing a shared connection.

Regression coverage: code_index lifecycle tests and examples/lsp_spike/tests.rs.
Remove the patch when upstream provides equivalent cancellation and record cleanup.
