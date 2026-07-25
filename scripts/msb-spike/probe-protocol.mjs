// Pearl th-a63c22 — host-side canonical-protocol probe against the REAL
// smooth-daemon running INSIDE the msb microVM (via the forwarded host port).
//
//   (a) create_conversation_session -> immediate_response{data.sessionId}
//   (b) send_message                -> eventual_response  (a real LLM turn
//       through the microVM's egress allowlist, proving --secret injection
//       and the allow rule work for real traffic)
//
// Usage: node probe-protocol.mjs [ws-url] [message]
//   default url: ws://127.0.0.1:18791/ws?token=spike-token
const url = process.argv[2] || 'ws://127.0.0.1:18791/ws?token=spike-token';
const message = process.argv[3] || 'Reply with exactly: PONG';
const ws = new WebSocket(url);
let sessionId = null;
const t = setTimeout(() => {
    console.log('FAIL: timeout (sessionId=' + sessionId + ')');
    process.exit(1);
}, 120000);

ws.onopen = () => {
    console.log('WS OPEN ->', url);
    const frame = { action: 'create_conversation_session', requestId: 'spike-cs', agentId: crypto.randomUUID(), userName: 'spike' };
    console.log('SENT   ->', JSON.stringify(frame));
    ws.send(JSON.stringify(frame));
};
ws.onmessage = (e) => {
    console.log('RECV   <-', String(e.data).slice(0, 2000));
    const v = JSON.parse(e.data);
    if (!sessionId && v.type === 'immediate_response' && v.data?.sessionId) {
        sessionId = v.data.sessionId;
        console.log('PASS(a): handshake sessionId =', sessionId);
        const frame = { action: 'send_message', requestId: 'spike-msg', sessionId, message };
        console.log('SENT   ->', JSON.stringify(frame));
        ws.send(JSON.stringify(frame));
        return;
    }
    if (v.type === 'eventual_response') {
        console.log('PASS(b): eventual_response received — the LLM turn reached the gateway.');
        clearTimeout(t);
        ws.close();
        process.exit(0);
    }
    if (v.type === 'error') {
        console.log('FAIL(b): error frame');
        clearTimeout(t);
        ws.close();
        process.exit(1);
    }
};
ws.onerror = (e) => {
    console.log('FAIL: ws error', e.message ?? e);
    process.exit(1);
};
ws.onclose = (e) => {
    console.log('WS CLOSE', e.code, e.reason);
    if (!sessionId) process.exit(1);
};
