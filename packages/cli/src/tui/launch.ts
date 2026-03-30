/** TUI launcher — dynamic import to avoid loading React for non-TUI commands */

export async function launchTui(serverUrl?: string): Promise<void> {
    const React = await import('react');
    const { render } = await import('ink');
    const { App } = await import('./App.js');

    render(React.createElement(App, { serverUrl }));
}
