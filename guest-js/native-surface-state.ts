import { VIDEO_CONTROLS_ATTRIBUTE } from '@get-air/video/controls'

import type { VisibleSurfaceBounds } from './native-surface-layout'
import { ZERO_RADIUS_STYLES, type CornerRadiusStyles, type Rect } from './native-surface-geometry'
import {
  applyBackground,
  commitOccluder,
  inlineProperty,
  readBackground,
  readCornerRadiusStyles,
  restoreInlineProperty,
} from './native-surface-paint'

export interface BackgroundPanel {
  clip: HTMLDivElement
  paint: HTMLDivElement
}

export interface DrilledAncestor {
  element: HTMLElement
  previousOwner: string | null
  mirror: HTMLDivElement
  panels: BackgroundPanel[]
  clipsX: boolean
  clipsY: boolean
  borderLeft: number
  borderTop: number
  borderRight: number
  borderBottom: number
  radii: CornerRadiusStyles
  paintsBackground: boolean
}

export interface BackgroundSnapshot {
  properties: readonly [string, string][]
  borderRadius: string
  clipsX: boolean
  clipsY: boolean
  borderLeft: number
  borderTop: number
  borderRight: number
  borderBottom: number
  radii: CornerRadiusStyles
  paintsBackground: boolean
}

export interface ClippedOccluder {
  element: HTMLElement
  owner: string
  active: boolean
  previousOwner: string | null
  previousImage: InlinePropertySnapshot
  previousPosition: InlinePropertySnapshot
  previousSize: InlinePropertySnapshot
}

export interface InlinePropertySnapshot {
  value: string
  priority: string
}

export interface NativeCssSurfaceState {
  owner: string
  anchor: HTMLVideoElement
  layer: HTMLDivElement
  style: HTMLStyleElement
  drilled: DrilledAncestor[]
  rootHadClass: boolean
  previousSession: string | undefined
  anchorRadii: CornerRadiusStyles
  occluders: ClippedOccluder[]
  protectedElements: Set<HTMLElement>
  lastBounds?: VisibleSurfaceBounds
}

export const nativeCssSurfaceScope = globalThis as typeof globalThis & {
  __TAURI_VIDEO_NATIVE_CSS_SURFACE__?: NativeCssSurfaceState
}

export const committedStyleValues = new WeakMap<HTMLElement, Map<string, string>>()
export const OCCLUDER_ATTRIBUTE = 'data-tauri-native-video-occluder'
export const MASK_IMAGE_PROPERTY = '--tauri-native-video-mask-image'
export const MASK_POSITION_PROPERTY = '--tauri-native-video-mask-position'
export const MASK_SIZE_PROPERTY = '--tauri-native-video-mask-size'

const NATIVE_VIDEO_CSS_PROPERTIES = [
  '--tauri-native-video-left',
  '--tauri-native-video-top',
  '--tauri-native-video-right',
  '--tauri-native-video-bottom',
  '--tauri-native-video-width',
  '--tauri-native-video-height',
] as const

export function claimSurface(owner: string, anchor: HTMLVideoElement): void {
  const existing = nativeCssSurfaceScope.__TAURI_VIDEO_NATIVE_CSS_SURFACE__
  if (existing?.anchor === anchor && existing.layer.isConnected) {
    for (const ancestor of existing.drilled) {
      if (ancestor.element.getAttribute('data-tauri-native-video-hole') === existing.owner) {
        ancestor.element.setAttribute('data-tauri-native-video-hole', owner)
      }
    }
    for (const occluder of existing.occluders) {
      if (occluder.element.getAttribute(OCCLUDER_ATTRIBUTE) === existing.owner) {
        occluder.element.setAttribute(OCCLUDER_ATTRIBUTE, owner)
      }
      occluder.owner = owner
    }
    existing.owner = owner
    existing.style.textContent = holeStyle(owner)
    document.documentElement.dataset.tauriNativeVideoSession = owner
    return
  }
  if (existing) releaseSurface(existing.owner)
  removeOrphanedSurfaceNodes()

  const root = document.documentElement
  const layer = document.createElement('div')
  layer.dataset.tauriNativeVideoBackdrop = ''
  layer.setAttribute('aria-hidden', 'true')
  Object.assign(layer.style, {
    position: 'fixed',
    zIndex: '-2147483647',
    inset: '0',
    overflow: 'hidden',
    pointerEvents: 'none',
    contain: 'strict',
  })
  const style = document.createElement('style')
  style.dataset.tauriNativeVideoBackdropStyle = ''
  style.textContent = holeStyle(owner)
  layer.append(style)
  document.body.prepend(layer)

  const state: NativeCssSurfaceState = {
    owner,
    anchor,
    layer,
    style,
    drilled: [],
    rootHadClass: root.classList.contains('tauri-native-video'),
    previousSession: root.dataset.tauriNativeVideoSession,
    anchorRadii: readCornerRadiusStyles(anchor),
    occluders: [],
    protectedElements: new Set(),
  }
  rebuildAncestors(state, collectAncestors(anchor))
  root.dataset.tauriNativeVideoSession = owner
  root.classList.add('tauri-native-video')
  nativeCssSurfaceScope.__TAURI_VIDEO_NATIVE_CSS_SURFACE__ = state
  rebuildOccluders(state)
}

export function rebuildAncestors(state: NativeCssSurfaceState, elements: readonly HTMLElement[]): void {
  for (const ancestor of state.drilled) restoreAncestor(ancestor, state.owner)
  state.drilled = []
  // The outer background paints first, then progressively more local surfaces.
  for (const element of [...elements].reverse()) {
    const ancestor = createAncestor(element, state.owner)
    state.drilled.unshift(ancestor)
    state.layer.append(ancestor.mirror)
  }
}

export function rebuildOccluders(state: NativeCssSurfaceState): void {
  for (const occluder of state.occluders) restoreOccluder(occluder)
  state.occluders = []
  const protectedElements = new Set<HTMLElement>()
  protectPath(state.anchor, protectedElements)
  for (const controls of document.querySelectorAll<HTMLElement>(`[${VIDEO_CONTROLS_ATTRIBUTE}]`)) {
    protectPath(controls, protectedElements)
  }
  state.protectedElements = protectedElements

  const visit = (element: HTMLElement) => {
    if (element === state.layer || !element.isConnected) return
    if (element === state.anchor || element.hasAttribute(VIDEO_CONTROLS_ATTRIBUTE)) return
    if (protectedElements.has(element)) {
      for (const child of element.children) {
        if (child instanceof HTMLElement) visit(child)
      }
      return
    }
    if (element instanceof HTMLScriptElement || element instanceof HTMLStyleElement) return
    state.occluders.push({
      element,
      owner: state.owner,
      active: false,
      previousOwner: element.getAttribute(OCCLUDER_ATTRIBUTE),
      previousImage: inlineProperty(element, MASK_IMAGE_PROPERTY),
      previousPosition: inlineProperty(element, MASK_POSITION_PROPERTY),
      previousSize: inlineProperty(element, MASK_SIZE_PROPERTY),
    })
  }
  for (const child of document.body.children) {
    if (child instanceof HTMLElement) visit(child)
  }
  if (state.lastBounds) {
    for (const occluder of state.occluders) {
      commitOccluder(
        occluder,
        rectFrom(occluder.element.getBoundingClientRect()),
        state.lastBounds,
      )
    }
  }
}

function protectPath(element: HTMLElement, protectedElements: Set<HTMLElement>): void {
  for (let current: HTMLElement | null = element; current; current = current.parentElement) {
    protectedElements.add(current)
  }
}

export function refreshActiveOccluders(): void {
  const state = nativeCssSurfaceScope.__TAURI_VIDEO_NATIVE_CSS_SURFACE__
  if (state) rebuildOccluders(state)
}

function createAncestor(element: HTMLElement, owner: string): DrilledAncestor {
  const mirror = document.createElement('div')
  mirror.dataset.tauriNativeVideoBackground = ''
  Object.assign(mirror.style, {
    position: 'fixed',
    top: '0',
    left: '0',
    overflow: 'hidden',
    pointerEvents: 'none',
  })
  const ancestor = {
    element,
    previousOwner: element.getAttribute('data-tauri-native-video-hole'),
    mirror,
    panels: [],
    clipsX: false,
    clipsY: false,
    borderLeft: 0,
    borderTop: 0,
    borderRight: 0,
    borderBottom: 0,
    radii: ZERO_RADIUS_STYLES,
    paintsBackground: false,
  }
  const background = readBackground(element)
  element.setAttribute('data-tauri-native-video-hole', owner)
  applyBackground(ancestor, background)
  return ancestor
}

export function releaseSurface(owner: string): void {
  const state = nativeCssSurfaceScope.__TAURI_VIDEO_NATIVE_CSS_SURFACE__
  if (!state || state.owner !== owner) return
  for (const ancestor of state.drilled) restoreAncestor(ancestor, owner)
  for (const occluder of state.occluders) restoreOccluder(occluder)
  state.layer.remove()
  const root = document.documentElement
  if (state.previousSession === undefined) delete root.dataset.tauriNativeVideoSession
  else root.dataset.tauriNativeVideoSession = state.previousSession
  if (!state.rootHadClass) root.classList.remove('tauri-native-video')
  for (const property of NATIVE_VIDEO_CSS_PROPERTIES) root.style.removeProperty(property)
  delete nativeCssSurfaceScope.__TAURI_VIDEO_NATIVE_CSS_SURFACE__
}

function restoreAncestor(ancestor: DrilledAncestor, owner: string): void {
  if (ancestor.element.getAttribute('data-tauri-native-video-hole') === owner) {
    if (ancestor.previousOwner === null) ancestor.element.removeAttribute('data-tauri-native-video-hole')
    else ancestor.element.setAttribute('data-tauri-native-video-hole', ancestor.previousOwner)
  }
  ancestor.mirror.remove()
}

export function restoreOccluder(occluder: ClippedOccluder): void {
  if (!occluder.active) return
  if (occluder.element.getAttribute(OCCLUDER_ATTRIBUTE) === occluder.owner) {
    if (occluder.previousOwner === null) occluder.element.removeAttribute(OCCLUDER_ATTRIBUTE)
    else occluder.element.setAttribute(OCCLUDER_ATTRIBUTE, occluder.previousOwner)
  }
  restoreInlineProperty(occluder.element, MASK_IMAGE_PROPERTY, occluder.previousImage)
  restoreInlineProperty(occluder.element, MASK_POSITION_PROPERTY, occluder.previousPosition)
  restoreInlineProperty(occluder.element, MASK_SIZE_PROPERTY, occluder.previousSize)
  occluder.active = false
}

export function collectAncestors(anchor: HTMLElement): HTMLElement[] {
  const elements: HTMLElement[] = []
  for (let element = anchor.parentElement; element; element = element.parentElement) {
    elements.push(element)
  }
  return elements
}

function removeOrphanedSurfaceNodes(): void {
  for (const orphan of document.querySelectorAll('[data-tauri-native-video-backdrop]')) orphan.remove()
  for (const orphan of document.querySelectorAll('[data-tauri-native-video-hole]')) {
    orphan.removeAttribute('data-tauri-native-video-hole')
  }
}

function holeStyle(owner: string): string {
  return `
    [data-tauri-native-video-hole="${owner}"] { background: transparent !important; }
    body[data-tauri-native-video-hole="${owner}"] { position: relative !important; isolation: isolate !important; }
    [${OCCLUDER_ATTRIBUTE}="${owner}"] {
      -webkit-mask-image: var(${MASK_IMAGE_PROPERTY}) !important;
      -webkit-mask-position: var(${MASK_POSITION_PROPERTY}) !important;
      -webkit-mask-size: var(${MASK_SIZE_PROPERTY}) !important;
      -webkit-mask-repeat: no-repeat !important;
      -webkit-mask-origin: border-box !important;
      -webkit-mask-clip: border-box !important;
      -webkit-mask-composite: source-over !important;
      mask-image: var(${MASK_IMAGE_PROPERTY}) !important;
      mask-position: var(${MASK_POSITION_PROPERTY}) !important;
      mask-size: var(${MASK_SIZE_PROPERTY}) !important;
      mask-repeat: no-repeat !important;
      mask-origin: border-box !important;
      mask-clip: border-box !important;
      mask-composite: add !important;
      mask-mode: alpha !important;
    }
  `
}

export function rectFrom(rect: DOMRect | DOMRectReadOnly): Rect {
  return { left: rect.left, top: rect.top, width: rect.width, height: rect.height }
}
