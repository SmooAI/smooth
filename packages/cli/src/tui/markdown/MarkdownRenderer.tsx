/** MarkdownRenderer — Ink component for rendering markdown content
 *
 * Usage:
 *   <MarkdownRenderer content="# Hello\n\nSome **bold** text" />
 *
 * Supports streaming: re-render with updated content as chunks arrive.
 */

import React from 'react';

import { renderMarkdown } from './renderMarkdown.js';
import { DEFAULT_THEME, type MarkdownTheme } from './types.js';

interface MarkdownRendererProps {
    content: string;
    theme?: MarkdownTheme;
}

export function MarkdownRenderer({ content, theme = DEFAULT_THEME }: MarkdownRendererProps) {
    return <>{renderMarkdown(content, theme)}</>;
}
