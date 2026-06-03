// Objective-C++ shim over the PRIVATE CoreGraphics `CGVirtualDisplay` family.
//
// These classes are undocumented and have no public header, so we re-declare the
// interfaces we need (shapes taken from reverse-engineered headers and the behaviour
// of projects like KhaosT/CGVirtualDisplay and BetterDisplay). The Objective-C runtime
// resolves the real classes (shipped inside CoreGraphics) at launch.
//
// Exposed to Rust as a tiny C ABI: create returns a retained opaque handle whose
// lifetime owns the on-screen virtual display; destroy releases it.

#import <CoreGraphics/CoreGraphics.h>
#import <Foundation/Foundation.h>

// --- Private interface re-declarations -------------------------------------------------

@interface CGVirtualDisplayDescriptor : NSObject
@property(strong) NSString *name;
@property unsigned int maxPixelsWide;
@property unsigned int maxPixelsHigh;
@property CGSize sizeInMillimeters;
@property unsigned int productID;
@property unsigned int vendorID;
@property unsigned int serialNum;
@property(strong) dispatch_queue_t queue;
@property(copy) void (^terminationHandler)(id, id);
@end

@interface CGVirtualDisplayMode : NSObject
- (instancetype)initWithWidth:(unsigned int)width
                       height:(unsigned int)height
                  refreshRate:(double)refreshRate;
@end

@interface CGVirtualDisplaySettings : NSObject
@property(strong) NSArray<CGVirtualDisplayMode *> *modes;
@property unsigned int hiDPI;
@end

@interface CGVirtualDisplay : NSObject
- (instancetype)initWithDescriptor:(CGVirtualDisplayDescriptor *)descriptor;
- (BOOL)applySettings:(CGVirtualDisplaySettings *)settings;
@property(readonly) unsigned int displayID;
@end

// --- C ABI exposed to Rust ------------------------------------------------------------

#ifdef __cplusplus
extern "C" {
#endif

// Creates a virtual display of `width`x`height` at `refresh` Hz.
// On success returns a retained opaque handle and writes the CGDirectDisplayID to
// *out_display_id; on failure returns NULL.
void *rs_vdisplay_create(unsigned int width,
                         unsigned int height,
                         double refresh,
                         unsigned int *out_display_id) {
  @autoreleasepool {
    Class descCls = NSClassFromString(@"CGVirtualDisplayDescriptor");
    Class modeCls = NSClassFromString(@"CGVirtualDisplayMode");
    Class setCls = NSClassFromString(@"CGVirtualDisplaySettings");
    Class dispCls = NSClassFromString(@"CGVirtualDisplay");
    if (!descCls || !modeCls || !setCls || !dispCls) {
      // The private classes are not present in this macOS build.
      return NULL;
    }

    CGVirtualDisplayDescriptor *desc = [[descCls alloc] init];
    desc.name = @"RustScreen";
    desc.maxPixelsWide = width;
    desc.maxPixelsHigh = height;
    // Physical size at ~110 ppi (1 px ≈ 0.231 mm). Only affects reported DPI, not creation.
    desc.sizeInMillimeters = CGSizeMake(width * 0.231, height * 0.231);
    desc.productID = 0x0001;
    desc.vendorID = 0x726D; // 'rm' — arbitrary, identifies RustScreen displays
    desc.serialNum = 0x0001;
    desc.queue = dispatch_queue_create("com.rustscreen.vdisplay", DISPATCH_QUEUE_SERIAL);
    desc.terminationHandler = ^(id a, id b) {
      (void)a;
      (void)b;
    };

    CGVirtualDisplay *disp = [[dispCls alloc] initWithDescriptor:desc];
    if (!disp) {
      return NULL;
    }

    CGVirtualDisplayMode *mode =
        [[modeCls alloc] initWithWidth:width height:height refreshRate:refresh];
    CGVirtualDisplaySettings *settings = [[setCls alloc] init];
    settings.modes = @[ mode ];
    settings.hiDPI = 0;

    if (![disp applySettings:settings]) {
      return NULL; // ARC releases disp
    }

    if (out_display_id) {
      *out_display_id = disp.displayID;
    }
    // Transfer ownership to the caller; the display lives until rs_vdisplay_destroy.
    return (void *)CFBridgingRetain(disp);
  }
}

void rs_vdisplay_destroy(void *handle) {
  if (handle) {
    CFBridgingRelease(handle); // ARC releases the CGVirtualDisplay, tearing down the display
  }
}

// Number of active displays the window server currently reports (for spike verification).
unsigned int rs_active_display_count(void) {
  uint32_t count = 0;
  if (CGGetActiveDisplayList(0, NULL, &count) != kCGErrorSuccess) {
    return 0;
  }
  return count;
}

#ifdef __cplusplus
}
#endif
