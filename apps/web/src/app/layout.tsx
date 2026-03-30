import type { Metadata } from 'next';

import { LayoutDashboard, FolderKanban, Circle, Bot, MessageSquare, Mail, ShieldCheck, Settings } from 'lucide-react';

import './globals.css';

export const metadata: Metadata = {
    title: 'Smooth — AI Agent Orchestration',
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
                <div className="flex min-h-screen">
                    <nav className="w-56 border-r border-neutral-800 p-4 flex flex-col gap-1">
                        <div className="text-xl font-bold text-cyan-400 mb-4 px-3">SMOOTH</div>
                        {NAV_ITEMS.map(({ href, label, icon: Icon }) => (
                            <a
                                key={href}
                                href={href}
                                className="flex items-center gap-3 px-3 py-2 rounded-md text-sm text-neutral-400 hover:text-neutral-100 hover:bg-neutral-800/50 transition-colors"
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
