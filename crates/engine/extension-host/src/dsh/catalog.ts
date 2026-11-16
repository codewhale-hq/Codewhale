import { Service } from '@deepseek-ai/cordis'
import { ErrorCode } from '../protocol.ts'
import { RpcError } from '../rpc.ts'
import type { AdapterHost, DshAdapter } from './adapters.ts'

export interface ApprovedRoute {
  provider: string
  base_url: string
  default_model: string | null
  models: string[]
}

export interface ResolvedRoute extends ApprovedRoute {
  model: string | null
}

export interface InstallPlan {
  status: 'review_required' | 'already_present'
  package: string
  version: string
  plugin: string
  spec?: string
  command: string
  message: string
}

export type BalanceAnswer =
  | { status: 'known', currency: string, total: string, topped_up: string, granted: string }
  | { status: 'unknown' }
  | { status: 'absent' }

interface AccountWallet {
  currency: string
  balance: string
}

type AccountBalance = { status: 'ready', value: AccountWallet[], bonusWallets: AccountWallet[] } | { status: 'failed' }

interface ChangeResult {
  changed: false
  application: 'failed'
  stage: 'install' | 'remove'
  target: string
  error: { code: string, diagnostic: string, message: string }
}

const EXACT_VERSION_ONLY = 'Codewhale plans a plugin install only for one exact npm version, written name@1.2.3'
const REMOVE_IN_CODEWHALE = 'Codewhale removes a plugin only from its own plugin manager, after you disable it: /plugin uninstall'
const WITHHELD_ACCOUNT_METHODS = [
  'getState', 'getProfile', 'getUnnotifiedBonuses', 'ackBonusNotified', 'startSignIn', 'cancelSignIn', 'signOut', 'watch',
  'resolveToken', 'rejectToken', 'getPlatformSession', 'getDeviceIdentity',
] as const

function invalid(what: string): RpcError {
  return new RpcError(ErrorCode.Internal, `invalid catalog broker ${what}`)
}

function record(value: unknown, what: string): Record<string, unknown> {
  if (value === null || typeof value !== 'object' || Array.isArray(value)) throw invalid(what)
  return value as Record<string, unknown>
}

function text(value: unknown, what: string): string {
  if (typeof value !== 'string' || value.length === 0) throw invalid(what)
  return value
}

function named(value: unknown, what: string): string {
  if (typeof value !== 'string' || value.length === 0) throw new RpcError(ErrorCode.InvalidParams, `${what} is a non-empty string`)
  return value
}

function approvedRoute(value: unknown): ApprovedRoute {
  const row = record(value, 'route')
  const models = row.models
  const defaultModel = row.default_model
  if (!Array.isArray(models) || models.some((model) => typeof model !== 'string') || (defaultModel !== null && typeof defaultModel !== 'string')) throw invalid('route')
  return { provider: text(row.provider, 'route'), base_url: text(row.base_url, 'route'), default_model: defaultModel, models: [...models] }
}

export async function approvedRoutes(host: AdapterHost, ctx: unknown): Promise<ApprovedRoute[]> {
  const routes = record(await host.broker(ctx, 'catalog', 'routes.list', {}), 'routes').routes
  if (!Array.isArray(routes)) throw invalid('routes')
  return routes.map(approvedRoute)
}

export async function resolveRoute(host: AdapterHost, ctx: unknown, provider: unknown, model?: unknown): Promise<ResolvedRoute> {
  const input = model === undefined || model === null
    ? { provider: named(provider, 'a provider route') }
    : { provider: named(provider, 'a provider route'), model: named(model, 'a model') }
  const answer = record(await host.broker(ctx, 'catalog', 'routes.resolve', input), 'route')
  const resolved = answer.model
  if (resolved !== null && typeof resolved !== 'string') throw invalid('route')
  return { ...approvedRoute(answer.route), model: resolved }
}

export async function accountBalance(host: AdapterHost, ctx: unknown): Promise<BalanceAnswer> {
  const answer = record(await host.broker(ctx, 'catalog', 'balance', {}), 'balance')
  const status = answer.status
  if (status === 'unknown' || status === 'absent') return { status }
  const toppedUp = answer.topped_up
  const granted = answer.granted
  if (status !== 'known' || typeof toppedUp !== 'string' || typeof granted !== 'string') throw invalid('balance')
  return { status, currency: text(answer.currency, 'balance'), total: text(answer.total, 'balance'), topped_up: toppedUp, granted }
}

export async function planInstall(host: AdapterHost, ctx: unknown, spec: unknown, registry?: unknown): Promise<InstallPlan> {
  const exact = typeof spec === 'string' ? spec.lastIndexOf('@') : -1
  if (typeof spec !== 'string' || exact <= 0 || exact === spec.length - 1) throw new RpcError(ErrorCode.InvalidParams, EXACT_VERSION_ONLY)
  const request = { package: spec.slice(0, exact), version: spec.slice(exact + 1) }
  const input = registry === undefined || registry === null ? request : { ...request, registry: named(registry, 'a registry URL') }
  const plan = record(await host.broker(ctx, 'catalog', 'install.plan', input), 'install plan')
  const status = plan.status
  const planned = plan.spec
  if ((status !== 'review_required' && status !== 'already_present') || (planned !== undefined && typeof planned !== 'string')) throw invalid('install plan')
  return {
    status,
    package: text(plan.package, 'install plan'),
    version: text(plan.version, 'install plan'),
    plugin: text(plan.plugin, 'install plan'),
    ...(planned === undefined ? {} : { spec: planned }),
    command: text(plan.command, 'install plan'),
    message: text(plan.message, 'install plan'),
  }
}

function notChanged(stage: ChangeResult['stage'], target: unknown, code: string, diagnostic: string): ChangeResult {
  return { changed: false, application: 'failed', stage, target: typeof target === 'string' ? target : '', error: { code, diagnostic, message: diagnostic } }
}

function declined(error: unknown): string | undefined {
  if (!(error instanceof Error)) return undefined
  if (error instanceof RpcError && error.code === ErrorCode.InvalidParams && error.data === undefined) return error.message
  const refusal = (error as { data?: { dsh_refusal?: unknown } }).data?.dsh_refusal
  return refusal === 'refused' || refusal === 'invalid_input' ? error.message : undefined
}

export const pluginManagerAdapter: DshAdapter = {
  service: 'pluginManager',
  define(host) {
    class PluginManagerService extends Service {
      constructor(ctx: any) { super(ctx, 'pluginManager') }

      async installBundle(spec: unknown, options?: { registry?: unknown } | null): Promise<ChangeResult> {
        try {
          const plan = await planInstall(host, this.ctx, spec, options?.registry)
          return notChanged('install', spec, plan.status === 'already_present' ? 'already-installed' : 'management-required', plan.message)
        } catch (error) {
          const diagnostic = declined(error)
          if (diagnostic === undefined) throw error
          return notChanged('install', spec, 'invalid-spec', diagnostic)
        }
      }

      async removeBundle(target: unknown): Promise<ChangeResult> {
        return notChanged('remove', target, 'management-required', REMOVE_IN_CODEWHALE)
      }

      async cancelInstall(): Promise<{ status: 'not-running' }> {
        return { status: 'not-running' }
      }
    }
    Object.freeze(PluginManagerService.prototype)
    return PluginManagerService
  },
}

export const deepseekAccountAdapter: DshAdapter = {
  service: 'deepseekAccount',
  define(host) {
    class DeepSeekAccountService extends Service {
      constructor(ctx: any) { super(ctx, 'deepseekAccount') }

      async getBalance(): Promise<AccountBalance | null> {
        const answer = await accountBalance(host, this.ctx)
        if (answer.status === 'absent') return null
        if (answer.status === 'unknown') return { status: 'failed' }
        const { currency, total, topped_up: toppedUp, granted } = answer
        const wallet = (balance: string): AccountWallet[] => balance.length > 0 ? [{ currency, balance }] : []
        return { status: 'ready', value: wallet(toppedUp.length > 0 ? toppedUp : total), bonusWallets: wallet(granted) }
      }
    }
    for (const method of WITHHELD_ACCOUNT_METHODS) {
      Object.defineProperty(DeepSeekAccountService.prototype, method, {
        value() {
          throw new RpcError(ErrorCode.NotAvailable, `deepseekAccount.${method} is not provided: Codewhale answers a read-only balance and never gives a plugin an account credential or a sign-in`)
        },
      })
    }
    Object.freeze(DeepSeekAccountService.prototype)
    return DeepSeekAccountService
  },
}
