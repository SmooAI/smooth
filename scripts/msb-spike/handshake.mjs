// Host-side canonical-protocol probe: connect to the operator LocalServer
// running INSIDE the msb microVM (via the forwarded host port) and complete
// create_conversation_session -> immediate_response{data.sessionId}.
const url = process.argv[2] || 'ws://127.0.0.1:18791/ws';
const ws = new WebSocket(url);
const t = setTimeout(() => {
    console.log('FAIL: timeout');
    process.exit(1);
}, 15000);

ws.onopen = () => {
    console.log('WS OPEN ->', url);
    const frame = {
        action: 'create_conversation_session',
        requestId: 'spike-cs',
        agentId: crypto.randomUUID(),
        userName: 'spike',
    };
    console.log('SENT   ->', JSON.stringify(frame));
    ws.send(JSON.stringify(frame));
};
ws.onmessage = (e) => {
    console.log('RECV   <-', e.data);
    const v = JSON.parse(e.data);
    if (v.type === 'immediate_response' && v.data?.sessionId) {
        console.log('PASS: sessionId =', v.data.sessionId);
        clearTimeout(t);
        ws.close();
        process.exit(0);
    }
};
ws.onerror = (e) => {
    console.log('FAIL: ws error', e.message ?? e);
    process.exit(1);
};
