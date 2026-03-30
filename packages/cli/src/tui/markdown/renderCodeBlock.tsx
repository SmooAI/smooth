import type { Tokens } from 'marked';

/** Render code block tokens with optional basic syntax highlighting */

import { Box, Text } from 'ink';
import React from 'react';

import type { MarkdownTheme } from './types.js';

/** Basic keyword highlighting for common languages */
const KEYWORD_PATTERNS: Array<{ pattern: RegExp; color: string }> = [
    {
        pattern:
            /\b(const|let|var|function|class|interface|type|import|export|from|return|if|else|for|while|switch|case|break|default|new|this|async|await|throw|try|catch|finally|typeof|instanceof)\b/g,
        color: 'magenta',
    },
    { pattern: /\b(true|false|null|undefined|NaN|Infinity)\b/g, color: 'yellow' },
    { pattern: /(["'`])(?:(?!\1|\\).|\\.)*\1/g, color: 'green' },
    { pattern: /\/\/.*$/gm, color: 'gray' },
    { pattern: /\b(\d+\.?\d*)\b/g, color: 'cyan' },
];

function highlightLine(line: string): React.ReactNode[] {
    const segments: Array<{ text: string; color?: string; start: number; end: number }> = [];

    for (const { pattern, color } of KEYWORD_PATTERNS) {
        pattern.lastIndex = 0;
        let match: RegExpExecArray | null;
        while ((match = pattern.exec(line)) !== null) {
            segments.push({ text: match[0], color, start: match.index, end: match.index + match[0].length });
        }
    }

    // Sort by position, remove overlaps
    segments.sort((a, b) => a.start - b.start);
    const filtered: typeof segments = [];
    let lastEnd = 0;
    for (const seg of segments) {
        if (seg.start >= lastEnd) {
            filtered.push(seg);
            lastEnd = seg.end;
        }
    }

    // Build output
    const nodes: React.ReactNode[] = [];
    let pos = 0;
    for (let i = 0; i < filtered.length; i++) {
        const seg = filtered[i];
        if (pos < seg.start) {
            nodes.push(<Text key={`t${i}`}>{line.slice(pos, seg.start)}</Text>);
        }
        nodes.push(
            <Text key={`h${i}`} color={seg.color}>
                {seg.text}
            </Text>,
        );
        pos = seg.end;
    }
    if (pos < line.length) {
        nodes.push(<Text key="end">{line.slice(pos)}</Text>);
    }

    return nodes.length > 0 ? nodes : [<Text key="raw">{line}</Text>];
}

export function renderCodeBlock(token: Tokens.Code, theme: MarkdownTheme, key: number): React.ReactNode {
    const lines = token.text.split('\n');
    const lang = token.lang ?? '';

    return (
        <Box key={key} flexDirection="column" borderStyle="round" borderColor={theme.codeBlock.borderColor} paddingX={1} marginBottom={1}>
            {lang && <Text dimColor>{lang}</Text>}
            {lines.map((line, i) => (
                <Text key={i} color={theme.codeBlock.color}>
                    {lang ? highlightLine(line) : line}
                </Text>
            ))}
        </Box>
    );
}
