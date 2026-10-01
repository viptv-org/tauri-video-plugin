import type { VisibleSurfaceBounds } from './native-surface-layout'
import type { CornerRadiusStyles } from './native-surface-geometry'

// Shared native CSS surface model. State bookkeeping and paint helpers both
// depend on this leaf module so neither imports the other in a cycle.

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
