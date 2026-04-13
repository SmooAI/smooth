// Contract tests for a React app that talks to 4 polyglot backends.
//
// The agent is expected to create:
//   src/main.tsx — renders <App /> into #root
//   src/App.tsx  — a component with:
//     - A title (data-testid="title") containing "Smooth"
//     - A row of 4 backend-status cards, one per language:
//         (data-testid="backend-rust"),
//         (data-testid="backend-go"),
//         (data-testid="backend-typescript"),
//         (data-testid="backend-python")
//       Each card has a "Check" button (data-testid="check-<lang>") that
//       fetches `/api/<lang>/health` and displays the result text
//       (data-testid="status-<lang>") — "ok" on success, "error" on failure.
//     - A "Check all" button (data-testid="check-all") that pings every backend.
//     - An interactive counter:
//         (data-testid="count") showing the current count,
//         (data-testid="increment") and (data-testid="decrement") buttons.
//
// Tests use jsdom + vitest fetch mocks — no real backend is contacted.

import { describe, it, expect, afterEach, vi } from 'vitest';
import { render, screen, fireEvent, cleanup, waitFor } from '@testing-library/react';
import '@testing-library/jest-dom/vitest';
import App from '../src/App';

afterEach(() => {
    cleanup();
    vi.restoreAllMocks();
});

function mockFetchOk(body: unknown = { status: 'ok', version: '1.0.0' }) {
    return vi.spyOn(globalThis, 'fetch').mockImplementation(
        async () =>
            ({
                ok: true,
                status: 200,
                json: async () => body,
                text: async () => JSON.stringify(body),
            }) as unknown as Response,
    );
}

function mockFetchError() {
    return vi
        .spyOn(globalThis, 'fetch')
        .mockImplementation(async () => Promise.reject(new Error('network error')));
}

describe('Page chrome', () => {
    it('renders the title', () => {
        render(<App />);
        expect(screen.getByTestId('title')).toHaveTextContent('Smooth');
    });

    it('renders a card for every backend', () => {
        render(<App />);
        for (const lang of ['rust', 'go', 'typescript', 'python']) {
            expect(screen.getByTestId(`backend-${lang}`)).toBeInTheDocument();
            expect(screen.getByTestId(`check-${lang}`)).toBeInTheDocument();
        }
    });
});

describe('Single-backend health check', () => {
    it('calls /api/rust/health and shows ok', async () => {
        const spy = mockFetchOk();
        render(<App />);
        fireEvent.click(screen.getByTestId('check-rust'));
        await waitFor(() => {
            expect(screen.getByTestId('status-rust')).toHaveTextContent('ok');
        });
        expect(spy).toHaveBeenCalledWith(
            expect.stringContaining('/api/rust/health'),
            expect.any(Object),
        );
    });

    it('calls /api/go/health', async () => {
        const spy = mockFetchOk();
        render(<App />);
        fireEvent.click(screen.getByTestId('check-go'));
        await waitFor(() => {
            expect(screen.getByTestId('status-go')).toHaveTextContent('ok');
        });
        expect(spy).toHaveBeenCalledWith(
            expect.stringContaining('/api/go/health'),
            expect.any(Object),
        );
    });

    it('calls /api/typescript/health', async () => {
        const spy = mockFetchOk();
        render(<App />);
        fireEvent.click(screen.getByTestId('check-typescript'));
        await waitFor(() => {
            expect(screen.getByTestId('status-typescript')).toHaveTextContent('ok');
        });
        expect(spy).toHaveBeenCalledWith(
            expect.stringContaining('/api/typescript/health'),
            expect.any(Object),
        );
    });

    it('calls /api/python/health', async () => {
        const spy = mockFetchOk();
        render(<App />);
        fireEvent.click(screen.getByTestId('check-python'));
        await waitFor(() => {
            expect(screen.getByTestId('status-python')).toHaveTextContent('ok');
        });
        expect(spy).toHaveBeenCalledWith(
            expect.stringContaining('/api/python/health'),
            expect.any(Object),
        );
    });

    it('shows error when the backend is unreachable', async () => {
        mockFetchError();
        render(<App />);
        fireEvent.click(screen.getByTestId('check-rust'));
        await waitFor(() => {
            expect(screen.getByTestId('status-rust')).toHaveTextContent('error');
        });
    });
});

describe('Check all backends', () => {
    it('pings every backend and shows ok for each', async () => {
        const spy = mockFetchOk();
        render(<App />);
        fireEvent.click(screen.getByTestId('check-all'));
        await waitFor(() => {
            for (const lang of ['rust', 'go', 'typescript', 'python']) {
                expect(screen.getByTestId(`status-${lang}`)).toHaveTextContent('ok');
            }
        });
        expect(spy).toHaveBeenCalledTimes(4);
        const urls = spy.mock.calls.map((c) => String(c[0]));
        for (const lang of ['rust', 'go', 'typescript', 'python']) {
            expect(urls.some((u) => u.includes(`/api/${lang}/health`))).toBe(true);
        }
    });
});

describe('Counter', () => {
    it('starts at 0', () => {
        render(<App />);
        expect(screen.getByTestId('count')).toHaveTextContent('0');
    });

    it('increments and decrements', () => {
        render(<App />);
        const inc = screen.getByTestId('increment');
        const dec = screen.getByTestId('decrement');
        fireEvent.click(inc);
        fireEvent.click(inc);
        fireEvent.click(dec);
        expect(screen.getByTestId('count')).toHaveTextContent('1');
    });
});
