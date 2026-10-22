// cargo 转发器：把 cargo 挂上正确 PATH 后执行参数里的 cargo 命令
// 用法：node .cc-scratch/d005-cargo.mjs test --workspace
import { spawnSync } from 'node:child_process';

const cargoBin = 'C:\\Users\\Administrator\\.cargo\\bin';
const args = process.argv.slice(2);
const env = { ...process.env, PATH: cargoBin + ';' + (process.env.PATH || '') };
const r = spawnSync('cargo', args, { env, stdio: 'inherit', shell: false, cwd: 'D:\\projects\\CodeWorld' });
process.exit(r.status ?? 1);
