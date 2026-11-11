import { spawn } from 'node:child_process';
import { appendFileSync } from 'node:fs';

const [logPath, agentScript] = process.argv.slice(2);
const child = spawn(process.execPath, [agentScript], { stdio: ['pipe', 'pipe', 'inherit'], env: process.env });

function tap(tag) {
  let buffer = '';
  return (chunk) => {
    buffer += chunk;
    let index;
    while ((index = buffer.indexOf('\n')) >= 0) {
      const line = buffer.slice(0, index);
      buffer = buffer.slice(index + 1);
      if (line.trim()) appendFileSync(logPath, `${tag} ${line}\n`);
    }
  };
}

const fromClient = tap('C');
const fromAgent = tap('A');
process.stdin.setEncoding('utf8');
child.stdout.setEncoding('utf8');
process.stdin.on('data', (chunk) => {
  fromClient(chunk);
  child.stdin.write(chunk);
});
process.stdin.on('end', () => child.stdin.end());
child.stdout.on('data', (chunk) => {
  fromAgent(chunk);
  process.stdout.write(chunk);
});
child.on('exit', (code) => process.exit(code ?? 0));
