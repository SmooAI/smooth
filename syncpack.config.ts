import type { RcFile } from 'syncpack';

const config: RcFile = {
    source: ['package.json', 'apps/*/package.json', 'packages/*/package.json'],
    dependencyTypes: ['dev', 'prod', 'peer'],
    semverGroups: [],
    versionGroups: [],
};

export default config;
