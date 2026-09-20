import { VIDEO_CONTROLS_ATTRIBUTE } from '@get-air/video/controls'

import type { NativeCssSurfaceState } from './native-surface-state'

export function stylesheetMutation(record: MutationRecord): boolean {
  if (record.target instanceof HTMLStyleElement) return true
  return someChangedNode(record, (node) => (
    node instanceof HTMLStyleElement
    || (node instanceof HTMLLinkElement && node.rel === 'stylesheet')
  ))
}

export function structuralMutation(record: MutationRecord, state: NativeCssSurfaceState): boolean {
  if (record.type !== 'childList') return false
  if (record.target instanceof HTMLElement && state.protectedElements.has(record.target)) return true
  return someChangedNode(record, (node) => (
    node === state.anchor
    || (node instanceof Element && (
      node.contains(state.anchor)
      || node.matches(`[${VIDEO_CONTROLS_ATTRIBUTE}]`)
      || Boolean(node.querySelector(`[${VIDEO_CONTROLS_ATTRIBUTE}]`))
    ))
  ))
}

export function nativeCoordinateMutation(record: MutationRecord): boolean {
  if (record.target !== document.documentElement || record.attributeName !== 'style') return false
  return withoutNativeCoordinates(record.oldValue ?? '')
    === withoutNativeCoordinates(document.documentElement.getAttribute('style') ?? '')
}

export function withoutNativeCoordinates(style: string): string {
  return style
    .replace(/--tauri-native-video-(?:left|top|right|bottom|width|height)\s*:[^;]*(?:;|$)/gi, '')
    .replace(/\s+/g, '')
}

export function mutationContains(record: MutationRecord, anchor: HTMLElement): boolean {
  if (record.target instanceof Node && record.target.contains(anchor)) return true
  return someChangedNode(record, (node) => (
    node === anchor || (node instanceof Node && node.contains(anchor))
  ))
}

function someChangedNode(record: MutationRecord, predicate: (node: Node) => boolean): boolean {
  for (const node of record.addedNodes) if (predicate(node)) return true
  for (const node of record.removedNodes) if (predicate(node)) return true
  return false
}
