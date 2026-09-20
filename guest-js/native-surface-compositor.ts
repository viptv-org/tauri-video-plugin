import {
  visibleSurfaceBounds,
  type NativeSurfaceLayout,
  type VisibleSurfaceBounds,
} from './native-surface-layout'
import {
  intersectBounds,
  mergeIntersectionRadii,
  parseCornerRadii,
  radiiForIntersection,
  subtractRadius,
  ZERO_RADII,
  ZERO_RADIUS_STYLES,
  type CornerRadii,
  type Rect,
} from './native-surface-geometry'
import {
  registerVideoControls as registerAirVideoControls,
  type VideoControlsTarget,
} from '@get-air/video/controls'
import {
  claimSurface,
  collectAncestors,
  nativeCssSurfaceScope,
  rebuildAncestors,
  rebuildOccluders,
  rectFrom,
  refreshActiveOccluders,
  releaseSurface,
  type DrilledAncestor,
  type NativeCssSurfaceState,
} from './native-surface-state'
import {
  commitAncestor,
  commitOccluder,
  refreshBackgrounds,
  setProperty,
} from './native-surface-paint'
import {
  mutationContains,
  nativeCoordinateMutation,
  structuralMutation,
  stylesheetMutation,
} from './native-surface-mutation'

export { VIDEO_CONTROLS_ATTRIBUTE, type VideoControlsTarget } from '@get-air/video/controls'
export { outsidePanels } from './native-surface-geometry'

/**
 * Marks arbitrary DOM as intentional video UI. The element can live anywhere
 * in the document; it does not need to be a child of or overlap the video.
 */
export function registerVideoControls(target: VideoControlsTarget): () => void {
  const release = registerAirVideoControls(target)
  refreshActiveOccluders()
  let registered = true
  return () => {
    if (!registered) return
    registered = false
    release()
    refreshActiveOccluders()
  }
}

export interface SurfaceCompositorFrame {
  bounds: VisibleSurfaceBounds
  width: number
  height: number
  radii: CornerRadii
  ancestors: readonly ElementFrame[]
  occluders: readonly ElementFrame[]
}

interface ElementFrame {
  element: HTMLElement
  rect: Rect
}

/**
 * Owns the transparent aperture and exact background reconstruction around it.
 * Reads happen in `measure`; DOM writes happen in `commit`, keeping every
 * animation frame free of read/write layout thrashing.
 */
export class NativeSurfaceCompositor {
  readonly #owner: string
  readonly #anchor: HTMLVideoElement

  constructor(owner: string, anchor: HTMLVideoElement) {
    this.#owner = owner
    this.#anchor = anchor
    claimSurface(owner, anchor)
  }

  measure(layout: NativeSurfaceLayout, scale: number): SurfaceCompositorFrame {
    const state = this.#state()
    const surfaceBounds = {
      left: layout.x / scale,
      top: layout.y / scale,
      right: (layout.x + layout.width) / scale,
      bottom: (layout.y + layout.height) / scale,
    }
    let bounds = visibleSurfaceBounds(layout, scale, {
      width: window.innerWidth,
      height: window.innerHeight,
    })
    let radii = radiiForIntersection(
      bounds,
      surfaceBounds,
      parseCornerRadii(state?.anchorRadii ?? ZERO_RADIUS_STYLES, surfaceBounds),
    )
    if (!state) return {
      bounds,
      width: layout.width / scale,
      height: layout.height / scale,
      radii,
      ancestors: [],
      occluders: [],
    }
    const ancestors = state.drilled.map(({ element }) => ({
      element,
      rect: rectFrom(element.getBoundingClientRect()),
    }))
    const occluders = state.occluders.map(({ element }) => ({
      element,
      rect: rectFrom(element.getBoundingClientRect()),
    }))
    for (let index = 0; index < ancestors.length; index += 1) {
      const ancestor = state.drilled[index]
      const clip = ancestorClip(ancestors[index].rect, ancestor)
      if (!clip) continue
      const nextBounds = intersectBounds(bounds, clip)
      radii = mergeIntersectionRadii(
        bounds,
        radii,
        nextBounds,
        clip,
        innerCornerRadii(ancestor, ancestors[index].rect),
      )
      bounds = nextBounds
    }
    return {
      bounds,
      width: layout.width / scale,
      height: layout.height / scale,
      radii,
      ancestors,
      occluders,
    }
  }

  commit(frame: SurfaceCompositorFrame): void {
    const state = this.#state()
    if (!state) return
    state.lastBounds = frame.bounds
    for (let index = 0; index < state.drilled.length; index += 1) {
      const ancestor = state.drilled[index]
      const measured = frame.ancestors[index]
      if (measured?.element !== ancestor.element) continue
      commitAncestor(ancestor, measured.rect, frame.bounds, frame.radii)
    }
    for (let index = 0; index < state.occluders.length; index += 1) {
      const occluder = state.occluders[index]
      const measured = frame.occluders[index]
      if (measured?.element !== occluder.element) continue
      commitOccluder(occluder, measured.rect, frame.bounds)
    }
    const root = document.documentElement
    setProperty(root, '--tauri-native-video-left', `${frame.bounds.left}px`)
    setProperty(root, '--tauri-native-video-top', `${frame.bounds.top}px`)
    setProperty(root, '--tauri-native-video-right', `${frame.bounds.right}px`)
    setProperty(root, '--tauri-native-video-bottom', `${frame.bounds.bottom}px`)
    setProperty(root, '--tauri-native-video-width', `${frame.width}px`)
    setProperty(root, '--tauri-native-video-height', `${frame.height}px`)
  }

  /** Re-reads backgrounds and repairs the ancestor chain after DOM changes. */
  refresh(): void {
    const state = this.#state()
    if (!state) return
    const elements = collectAncestors(this.#anchor)
    const chainChanged = elements.length !== state.drilled.length
      || elements.some((element, index) => state.drilled[index]?.element !== element)
    if (chainChanged) rebuildAncestors(state, elements)
    else refreshBackgrounds(state)
    rebuildOccluders(state)
  }

  /**
   * Watches only mutations that can move the anchor or change a drilled
   * background. Unrelated component updates do not enter the layout path.
   */
  observe(onInvalidated: (backgroundChanged: boolean) => void): () => void {
    if (typeof MutationObserver === 'undefined') return () => undefined
    const observer = new MutationObserver((records) => {
      const state = this.#state()
      if (!state) return
      let layoutChanged = false
      let backgroundChanged = false
      for (const record of records) {
        if (record.type === 'attributes') {
          if (nativeCoordinateMutation(record)) continue
          if (record.target === this.#anchor
            || state.drilled.some(({ element }) => element === record.target)) {
            layoutChanged = true
            backgroundChanged = true
          }
          continue
        }
        if (stylesheetMutation(record) || structuralMutation(record, state)) backgroundChanged = true
        if (state.drilled.some(({ element }) => element === record.target)
          || mutationContains(record, this.#anchor)) {
          layoutChanged = true
        }
      }
      if (layoutChanged || backgroundChanged) onInvalidated(backgroundChanged)
    })
    observer.observe(document.documentElement, {
      attributes: true,
      attributeOldValue: true,
      attributeFilter: ['class', 'style'],
      childList: true,
      subtree: true,
    })
    return () => observer.disconnect()
  }

  release(): void {
    releaseSurface(this.#owner)
  }

  #state(): NativeCssSurfaceState | undefined {
    const state = nativeCssSurfaceScope.__TAURI_VIDEO_NATIVE_CSS_SURFACE__
    return state?.owner === this.#owner && state.anchor === this.#anchor ? state : undefined
  }
}

function ancestorClip(
  rect: Rect,
  ancestor: DrilledAncestor,
): VisibleSurfaceBounds | undefined {
  if (!ancestor.clipsX && !ancestor.clipsY) return undefined
  return {
    left: ancestor.clipsX ? rect.left + ancestor.borderLeft : Number.NEGATIVE_INFINITY,
    top: ancestor.clipsY ? rect.top + ancestor.borderTop : Number.NEGATIVE_INFINITY,
    right: ancestor.clipsX
      ? rect.left + rect.width - ancestor.borderRight
      : Number.POSITIVE_INFINITY,
    bottom: ancestor.clipsY
      ? rect.top + rect.height - ancestor.borderBottom
      : Number.POSITIVE_INFINITY,
  }
}

function innerCornerRadii(ancestor: DrilledAncestor, rect: Rect): CornerRadii {
  if (!ancestor.clipsX || !ancestor.clipsY) return ZERO_RADII
  const radii = parseCornerRadii(ancestor.radii, {
    left: rect.left,
    top: rect.top,
    right: rect.left + rect.width,
    bottom: rect.top + rect.height,
  })
  return [
    subtractRadius(radii[0], ancestor.borderLeft, ancestor.borderTop),
    subtractRadius(radii[1], ancestor.borderRight, ancestor.borderTop),
    subtractRadius(radii[2], ancestor.borderRight, ancestor.borderBottom),
    subtractRadius(radii[3], ancestor.borderLeft, ancestor.borderBottom),
  ]
}
