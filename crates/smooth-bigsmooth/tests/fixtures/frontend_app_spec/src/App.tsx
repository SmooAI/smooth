import React, { useState } from 'react';

const BACKENDS = ['rust', 'go', 'typescript', 'python'] as const;
type Backend = typeof BACKENDS[number];

export default function App() {
    const [statuses, setStatuses] = useState<Record<Backend, string>>({
        rust: '',
        go: '',
        typescript: '',
        python: '',
    });
    const [count, setCount] = useState(0);

    async function checkBackend(lang: Backend) {
        try {
            const res = await fetch(`/api/${lang}/health`, {});
            const data = await res.json();
            setStatuses(prev => ({ ...prev, [lang]: data.status ?? 'ok' }));
        } catch {
            setStatuses(prev => ({ ...prev, [lang]: 'error' }));
        }
    }

    async function checkAll() {
        await Promise.all(BACKENDS.map(lang => checkBackend(lang)));
    }

    return (
        <div>
            <h1 data-testid="title">Smooth Task Dashboard</h1>

            <div>
                {BACKENDS.map(lang => (
                    <div key={lang} data-testid={`backend-${lang}`}>
                        <span>{lang}</span>
                        <button data-testid={`check-${lang}`} onClick={() => checkBackend(lang)}>
                            Check
                        </button>
                        <span data-testid={`status-${lang}`}>{statuses[lang]}</span>
                    </div>
                ))}
            </div>

            <button data-testid="check-all" onClick={checkAll}>Check all</button>

            <div>
                <button data-testid="decrement" onClick={() => setCount(c => c - 1)}>-</button>
                <span data-testid="count">{count}</span>
                <button data-testid="increment" onClick={() => setCount(c => c + 1)}>+</button>
            </div>
        </div>
    );
}
