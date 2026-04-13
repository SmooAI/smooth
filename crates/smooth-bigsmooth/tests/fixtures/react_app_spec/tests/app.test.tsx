// Contract tests for a small React app.
//
// The agent is expected to create:
//   src/main.tsx — renders <App /> into #root
//   src/App.tsx  — the interactive component
//
// Tests use @testing-library/react to render the component in jsdom and
// verify interactions without a real browser. This is the frontend mirror
// of the Rust/Hono backend spec tests.

import { describe, it, expect, afterEach } from 'vitest';
import { render, screen, fireEvent, cleanup } from '@testing-library/react';
import '@testing-library/jest-dom/vitest';
import App from '../src/App';

afterEach(cleanup);

describe('App renders', () => {
    it('shows the app title', () => {
        render(<App />);
        expect(screen.getByTestId('title')).toHaveTextContent('Smooth');
    });
});

describe('Counter', () => {
    it('starts at 0', () => {
        render(<App />);
        expect(screen.getByTestId('count')).toHaveTextContent('0');
    });

    it('increments when the button is clicked', () => {
        render(<App />);
        const button = screen.getByTestId('increment');
        fireEvent.click(button);
        expect(screen.getByTestId('count')).toHaveTextContent('1');
        fireEvent.click(button);
        expect(screen.getByTestId('count')).toHaveTextContent('2');
    });

    it('decrements when the decrement button is clicked', () => {
        render(<App />);
        const inc = screen.getByTestId('increment');
        const dec = screen.getByTestId('decrement');
        fireEvent.click(inc);
        fireEvent.click(inc);
        fireEvent.click(dec);
        expect(screen.getByTestId('count')).toHaveTextContent('1');
    });
});

describe('Name form', () => {
    it('shows a text input and a submit button', () => {
        render(<App />);
        expect(screen.getByTestId('name-input')).toBeInTheDocument();
        expect(screen.getByTestId('submit-name')).toBeInTheDocument();
    });

    it('displays a greeting after submitting a name', () => {
        render(<App />);
        const input = screen.getByTestId('name-input') as HTMLInputElement;
        const submit = screen.getByTestId('submit-name');
        fireEvent.change(input, { target: { value: 'Smooth' } });
        fireEvent.click(submit);
        expect(screen.getByTestId('greeting')).toHaveTextContent('Hello, Smooth');
    });

    it('does not show a greeting before submission', () => {
        render(<App />);
        expect(screen.queryByTestId('greeting')).not.toBeInTheDocument();
    });
});

describe('Todo list', () => {
    it('can add an item', () => {
        render(<App />);
        const input = screen.getByTestId('todo-input') as HTMLInputElement;
        const add = screen.getByTestId('add-todo');
        fireEvent.change(input, { target: { value: 'Ship Smooth' } });
        fireEvent.click(add);
        expect(screen.getByText('Ship Smooth')).toBeInTheDocument();
    });

    it('can add multiple items', () => {
        render(<App />);
        const input = screen.getByTestId('todo-input') as HTMLInputElement;
        const add = screen.getByTestId('add-todo');
        fireEvent.change(input, { target: { value: 'First' } });
        fireEvent.click(add);
        fireEvent.change(input, { target: { value: 'Second' } });
        fireEvent.click(add);
        expect(screen.getByText('First')).toBeInTheDocument();
        expect(screen.getByText('Second')).toBeInTheDocument();
    });

    it('clears the input after adding', () => {
        render(<App />);
        const input = screen.getByTestId('todo-input') as HTMLInputElement;
        const add = screen.getByTestId('add-todo');
        fireEvent.change(input, { target: { value: 'Item' } });
        fireEvent.click(add);
        expect(input.value).toBe('');
    });
});

describe('Build output', () => {
    it('renders without crashing', () => {
        const { container } = render(<App />);
        expect(container.querySelector('[data-testid="title"]')).not.toBeNull();
    });
});
