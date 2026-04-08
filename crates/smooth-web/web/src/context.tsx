import { createContext, useContext, useState, useEffect, type ReactNode } from 'react';
import { api } from './api';

interface Project {
    path: string;
    name: string;
    pearl_counts: { open: number; in_progress: number; closed: number };
}

interface ProjectContextType {
    projects: Project[];
    selectedProject: string | null;
    setSelectedProject: (path: string) => void;
}

const ProjectContext = createContext<ProjectContextType>({
    projects: [],
    selectedProject: null,
    setSelectedProject: () => {},
});

export function useProject() {
    return useContext(ProjectContext);
}

export function ProjectProvider({ children }: { children: ReactNode }) {
    const [projects, setProjects] = useState<Project[]>([]);
    const [selectedProject, setSelectedProject] = useState<string | null>(null);

    useEffect(() => {
        api<{ data: Project[]; ok: boolean }>('/api/projects')
            .then((r) => {
                setProjects(r.data);
                if (r.data.length > 0 && !selectedProject) {
                    setSelectedProject(r.data[0].path);
                }
            })
            .catch(() => {});
    }, []);

    return (
        <ProjectContext.Provider value={{ projects, selectedProject, setSelectedProject }}>
            {children}
        </ProjectContext.Provider>
    );
}
