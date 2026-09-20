/**
 * @fileoverview Camoufox 安装目录环境
 * @description 必须在 import camoufox-js 之前加载。
 * camoufox-js@0.12 在模块加载时读取 CAMOUFOX_INSTALL_DIR。
 */

import path from 'path';
import { fileURLToPath } from 'url';

const PROJECT_ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../../..');

/** 项目内 Camoufox 安装目录（npm run init 落点） */
export const PROJECT_CAMOUFOX_DIR = path.join(PROJECT_ROOT, 'camoufox');

if (!process.env.CAMOUFOX_INSTALL_DIR) {
    process.env.CAMOUFOX_INSTALL_DIR = PROJECT_CAMOUFOX_DIR;
}

export const CAMOUFOX_INSTALL_DIR = process.env.CAMOUFOX_INSTALL_DIR;

export { PROJECT_ROOT };
