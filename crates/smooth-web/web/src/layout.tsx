import { LayoutDashboard, Circle, Bot, MessageSquare, Network, ChevronRight, LogIn, UserCheck } from 'lucide-react';
import { useEffect, useState } from 'react';
import { Link, Outlet, useLocation } from 'react-router-dom';

import { api } from './api';
import { Select } from './components/ui/select';
import { SidebarProvider, Sidebar, SidebarHeader, SidebarContent, SidebarTrigger, SidebarInset, useSidebar } from './components/ui/sidebar';
import { UiRelay } from './components/UiRelay';
import { useProject } from './context';

const NAV = [
    { path: '/', label: 'Dashboard', section: 'Overview', icon: LayoutDashboard },
    { path: '/pearls', label: 'Pearls', section: 'Work', icon: Circle },
    { path: '/operators', label: 'Operators', section: 'Work', icon: Bot },
    { path: '/chat', label: 'Chat', section: 'Tools', icon: MessageSquare },
    { path: '/system', label: 'System', section: 'Settings', icon: Network },
];

type AuthState = { loggedIn: boolean; user: string | null; orgId: string | null };

// Smoo AI sign-in affordance. Polls the daemon's /api/auth/status on
// load; if `th` isn't logged in, offers a plain anchor to /auth/login so
// the browser follows the daemon's 302 into the PKCE flow (no SSH needed).
function AuthStatus() {
    const [auth, setAuth] = useState<AuthState | null>(null);

    useEffect(() => {
        let alive = true;
        api<AuthState>('/api/auth/status')
            .then((a) => alive && setAuth(a))
            .catch(() => alive && setAuth({ loggedIn: false, user: null, orgId: null }));
        return () => {
            alive = false;
        };
    }, []);

    if (!auth) return null;

    if (auth.loggedIn) {
        return (
            <div className="flex items-center gap-2 px-1 text-xs text-muted-foreground">
                <UserCheck size={14} className="text-primary shrink-0" />
                <span className="truncate">Signed in{auth.user ? ` as ${auth.user}` : ''}</span>
            </div>
        );
    }

    return (
        <a
            href="/auth/login"
            className="flex items-center gap-2 px-3 py-2 rounded-md text-sm text-primary font-medium bg-primary/10 hover:bg-primary/20 border border-primary/20 transition-colors"
        >
            <LogIn size={16} />
            Sign in to Smoo AI
        </a>
    );
}

function Header() {
    const location = useLocation();
    const { open, isMobile } = useSidebar();
    const currentNav = NAV.find((n) => n.path === location.pathname) ?? NAV[0];
    const isSidebarOpen = isMobile ? open : open;

    return (
        <header className="flex h-14 shrink-0 items-center gap-2 border-b border-border px-4">
            <div className="flex w-full items-center justify-between">
                <div className="flex items-center gap-2">
                    <SidebarTrigger />
                    <div className="h-4 w-px bg-border mx-1" />
                    {/* Breadcrumbs */}
                    <nav className="flex items-center gap-1 text-sm">
                        <span className="text-muted-foreground hidden md:inline">Smooth</span>
                        <ChevronRight size={14} className="text-muted-foreground/50 hidden md:inline" />
                        <span className="text-muted-foreground hidden md:inline">{currentNav.section}</span>
                        <ChevronRight size={14} className="text-muted-foreground/50 hidden md:inline" />
                        <span className="font-medium">{currentNav.label}</span>
                    </nav>
                </div>
                {/* Logo — shows when sidebar is closed */}
                <img src="/logo.svg" alt="Smoo AI" className={`h-7 ${isSidebarOpen ? 'hidden' : 'md:hidden'}`} />
            </div>
        </header>
    );
}

export function Layout() {
    const location = useLocation();
    const { projects, selectedProject, setSelectedProject } = useProject();

    return (
        <SidebarProvider>
            <Sidebar>
                <SidebarHeader>
                    <div className="px-1">
                        <img src="/logo.svg" alt="Smoo AI" className="h-8" />
                    </div>

                    {projects.length > 0 && (
                        <div className="mt-2">
                            <label className="text-xs font-semibold uppercase tracking-wider text-muted-foreground mb-1.5 block px-1">Project</label>
                            <Select value={selectedProject ?? ''} onChange={(e) => setSelectedProject(e.target.value)}>
                                {projects.map((p) => (
                                    <option key={p.path} value={p.path}>
                                        {p.name}
                                    </option>
                                ))}
                            </Select>
                        </div>
                    )}

                    <div className="mt-2">
                        <AuthStatus />
                    </div>
                </SidebarHeader>

                <SidebarContent>
                    <div className="text-xs font-semibold uppercase tracking-wider text-muted-foreground px-3 mb-2">Smooth</div>
                    {NAV.map(({ path, label, icon: Icon }) => {
                        const active = location.pathname === path;
                        return (
                            <Link
                                key={path}
                                to={path}
                                className={
                                    'flex items-center gap-3 px-3 py-2 rounded-md text-sm transition-colors ' +
                                    (active
                                        ? 'text-primary font-semibold bg-primary/10 border-l-2 border-primary'
                                        : 'text-muted-foreground hover:text-foreground hover:bg-sidebar-accent border-l-2 border-transparent')
                                }
                            >
                                <Icon size={16} />
                                {label}
                            </Link>
                        );
                    })}
                </SidebarContent>
            </Sidebar>

            <SidebarInset>
                <Header />
                {/* min-h-0 + overflow-y-auto: the shell is fixed-height (h-dvh), so
                    content-heavy pages scroll HERE, under a pinned header, instead of
                    growing the document. Chat sizes itself to exactly this box
                    (h-[calc(100dvh-…)]) and manages its own inner scroll. Pearl th-ios-scroll. */}
                <main className="flex-1 min-h-0 overflow-y-auto p-4 md:p-6 min-w-0">
                    <Outlet />
                </main>
            </SidebarInset>
            {/* SEP Phase 6 — global overlay for extension ui/* frames */}
            <UiRelay />
        </SidebarProvider>
    );
}
