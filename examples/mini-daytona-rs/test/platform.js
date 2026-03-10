const os = require('node:os');
const path = require('node:path');

const CLIENT_ONLY = process.argv.includes('--client');
const PROJECT_ROOT = path.join(__dirname, '..');
const SANDBOX_WORKSPACE = '/home/daytona/workspace';

function isLocalMacOS() {
  return os.platform() === 'darwin';
}

function getBuildPath(subPath) {
  return CLIENT_ONLY ? subPath : path.join(PROJECT_ROOT, subPath);
}

function getSandboxPath(...parts) {
  return path.posix.join(SANDBOX_WORKSPACE, ...parts);
}

function getPythonCommand() {
  return 'python3';
}

module.exports = {
  CLIENT_ONLY,
  PROJECT_ROOT,
  SANDBOX_WORKSPACE,
  getBuildPath,
  getPythonCommand,
  getSandboxPath,
  isLocalMacOS,
};