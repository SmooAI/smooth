/** Beads-backed messaging types */

export type MessageDirection = 'leader→worker' | 'worker→leader' | 'worker→worker' | 'human→leader' | 'leader→human' | 'review' | 'progress' | 'artifact';

export interface Message {
    id: string;
    beadId: string;
    direction: MessageDirection;
    content: string;
    author: string;
    createdAt: string;
}

export interface InboxItem {
    message: Message;
    beadTitle: string;
    requiresAction: boolean;
    actionType?: 'approval' | 'review' | 'response' | 'info';
}

/** Message prefix convention for bead comments */
export const MESSAGE_PREFIXES: Record<MessageDirection, string> = {
    'leader→worker': '[leader→worker]',
    'worker→leader': '[worker→leader]',
    'worker→worker': '[worker→worker]',
    'human→leader': '[human→leader]',
    'leader→human': '[leader→human]',
    review: '[review]',
    progress: '[progress]',
    artifact: '[artifact]',
};

/** Artifact types stored in bead comments */
export type ArtifactType = 'diff' | 'test-results' | 'summary' | 'code' | 'document' | 'data';

export interface Artifact {
    type: ArtifactType;
    path: string;
    description?: string;
}
