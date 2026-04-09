import { Link, Outlet, useLocation } from "react-router-dom";
import {
    LayoutDashboard,
    Circle,
    Bot,
    MessageSquare,
    Settings,
    ChevronRight,
} from "lucide-react";
import { useProject } from "./context";
import { Select } from "./components/ui/select";
import {
    SidebarProvider,
    Sidebar,
    SidebarHeader,
    SidebarContent,
    SidebarTrigger,
    SidebarInset,
    useSidebar,
} from "./components/ui/sidebar";

const NAV = [
    { path: "/", label: "Dashboard", section: "Overview", icon: LayoutDashboard },
    { path: "/pearls", label: "Pearls", section: "Work", icon: Circle },
    { path: "/operators", label: "Operators", section: "Work", icon: Bot },
    { path: "/chat", label: "Chat", section: "Tools", icon: MessageSquare },
    { path: "/system", label: "System", section: "Settings", icon: Settings },
];

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
                        <span className="text-muted-foreground hidden md:inline">
                            Smooth
                        </span>
                        <ChevronRight
                            size={14}
                            className="text-muted-foreground/50 hidden md:inline"
                        />
                        <span className="text-muted-foreground hidden md:inline">
                            {currentNav.section}
                        </span>
                        <ChevronRight
                            size={14}
                            className="text-muted-foreground/50 hidden md:inline"
                        />
                        <span className="font-medium">{currentNav.label}</span>
                    </nav>
                </div>
                {/* Logo — shows when sidebar is closed */}
                <img
                    src="/logo.svg"
                    alt="Smoo AI"
                    className={`h-7 ${isSidebarOpen ? "hidden" : "md:hidden"}`}
                />
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
                            <label className="text-xs font-semibold uppercase tracking-wider text-muted-foreground mb-1.5 block px-1">
                                Project
                            </label>
                            <Select
                                value={selectedProject ?? ""}
                                onChange={(e) => setSelectedProject(e.target.value)}
                            >
                                {projects.map((p) => (
                                    <option key={p.path} value={p.path}>
                                        {p.name}
                                    </option>
                                ))}
                            </Select>
                        </div>
                    )}
                </SidebarHeader>

                <SidebarContent>
                    <div className="text-xs font-semibold uppercase tracking-wider text-muted-foreground px-3 mb-2">
                        Smooth
                    </div>
                    {NAV.map(({ path, label, icon: Icon }) => {
                        const active = location.pathname === path;
                        return (
                            <Link
                                key={path}
                                to={path}
                                className={
                                    "flex items-center gap-3 px-3 py-2 rounded-md text-sm transition-colors " +
                                    (active
                                        ? "text-primary font-semibold bg-primary/10 border-l-2 border-primary"
                                        : "text-muted-foreground hover:text-foreground hover:bg-sidebar-accent border-l-2 border-transparent")
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
                <main className="flex-1 p-6">
                    <Outlet />
                </main>
            </SidebarInset>
        </SidebarProvider>
    );
}
