import type { Metadata } from 'next';

import { Bot, Circle, FolderKanban, LayoutDashboard, Mail, MessageSquare, Settings, ShieldCheck } from 'lucide-react';
import Image from 'next/image';

import './globals.css';

export const metadata: Metadata = {
    title: 'Smooth — Smoo AI Agent Orchestration',
    description: 'Orchestrate Smooth Operators to work on any project',
};

const NAV_ITEMS = [
    { href: '/', label: 'Dashboard', icon: LayoutDashboard },
    { href: '/projects', label: 'Projects', icon: FolderKanban },
    { href: '/beads', label: 'Beads', icon: Circle },
    { href: '/operators', label: 'Operators', icon: Bot },
    { href: '/chat', label: 'Chat', icon: MessageSquare },
    { href: '/messages', label: 'Messages', icon: Mail },
    { href: '/reviews', label: 'Reviews', icon: ShieldCheck },
    { href: '/system', label: 'System', icon: Settings },
];

export default function RootLayout({ children }: { children: React.ReactNode }) {
    return (
        <html lang="en" className="dark">
            <body>
                <div className="flex min-h-screen" style={{ background: 'var(--background)' }}>
                    <nav className="w-56 p-4 flex flex-col gap-1" style={{ borderRight: '1px solid var(--border)' }}>
                        <div className="mb-4 px-3 py-1">
                            <Image src="/logo.svg" alt="Smoo AI" width={140} height={32} priority />
                        </div>
                        <div className="text-xs font-semibold uppercase tracking-wider px-3 mb-2" style={{ color: 'var(--muted)' }}>
                            Smooth
                        </div>
                        {NAV_ITEMS.map(({ href, label, icon: Icon }) => (
                            <a
                                key={href}
                                href={href}
                                className="flex items-center gap-3 px-3 py-2 rounded-md text-sm transition-colors"
                                style={{ color: 'var(--muted)' }}
                            >
                                <Icon size={16} />
                                {label}
                            </a>
                        ))}
                    </nav>
                    <main className="flex-1 p-6">{children}</main>
                </div>
            </body>
        </html>
    );
}
