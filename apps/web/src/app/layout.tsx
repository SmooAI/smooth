import type { Metadata } from 'next';

export const metadata: Metadata = {
    title: 'Smooth — AI Agent Orchestration',
    description: 'Orchestrate Smooth Operators to work on any project',
};

export default function RootLayout({ children }: { children: React.ReactNode }) {
    return (
        <html lang="en">
            <body style={{ margin: 0, fontFamily: 'system-ui, -apple-system, sans-serif', backgroundColor: '#0a0a0a', color: '#e5e5e5' }}>
                <div style={{ display: 'flex', minHeight: '100vh' }}>
                    <nav style={{ width: 220, borderRight: '1px solid #262626', padding: '16px', display: 'flex', flexDirection: 'column', gap: 4 }}>
                        <div style={{ fontSize: 20, fontWeight: 700, color: '#06b6d4', marginBottom: 16 }}>SMOOTH</div>
                        <NavLink href="/">Dashboard</NavLink>
                        <NavLink href="/projects">Projects</NavLink>
                        <NavLink href="/beads">Beads</NavLink>
                        <NavLink href="/operators">Operators</NavLink>
                        <NavLink href="/chat">Chat</NavLink>
                        <NavLink href="/messages">Messages</NavLink>
                        <NavLink href="/reviews">Reviews</NavLink>
                        <NavLink href="/system">System</NavLink>
                    </nav>
                    <main style={{ flex: 1, padding: 24 }}>{children}</main>
                </div>
            </body>
        </html>
    );
}

function NavLink({ href, children }: { href: string; children: React.ReactNode }) {
    return (
        <a
            href={href}
            style={{
                color: '#a3a3a3',
                textDecoration: 'none',
                padding: '8px 12px',
                borderRadius: 6,
                fontSize: 14,
            }}
        >
            {children}
        </a>
    );
}
