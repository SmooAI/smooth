import type { InferConfigTypes } from '@smooai/config/config';
import config from './config.ts';

type ConfigType = InferConfigTypes<typeof config>['ConfigTypeInput'];
const { FeatureFlagKeys: _FeatureFlagKeys } = config;

// Add per-key default values here, e.g.:
//   [_FeatureFlagKeys.MY_FLAG]: false,
export default {} satisfies ConfigType;
