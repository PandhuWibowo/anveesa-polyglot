// anveesa-pdfmask: reads/writes translation overlays in a PDF.
//
//   anveesa-pdfmask list <file.pdf>
//     → JSON {"lines":[{"page":0,"x":..,"y":..,"w":..,"h":..,"text":".."}]}
//       one entry per text line, coordinates in PDF page space (pt, origin
//       bottom-left) — exactly the space annotations use.
//
//   anveesa-pdfmask apply <file.pdf> <plan.json>
//     → adds a white free-text annotation with the translated text over each
//       given rect and saves the PDF IN PLACE. Original text is preserved
//       underneath (annotations are removable objects).
#import <Foundation/Foundation.h>
#import <PDFKit/PDFKit.h>
#import <AppKit/AppKit.h>

static int listLines(NSString *path) {
    PDFDocument *doc = [[PDFDocument alloc] initWithURL:[NSURL fileURLWithPath:path]];
    if (!doc) { fprintf(stderr, "could not open PDF\n"); return 1; }
    NSMutableArray *lines = [NSMutableArray array];
    for (NSUInteger p = 0; p < doc.pageCount; p++) {
        PDFPage *page = [doc pageAtIndex:p];
        PDFSelection *all = [page selectionForRange:NSMakeRange(0, page.string.length)];
        if (!all) continue;
        for (PDFSelection *line in all.selectionsByLine) {
            NSString *text = [line.string stringByTrimmingCharactersInSet:
                              NSCharacterSet.whitespaceAndNewlineCharacterSet];
            if (text.length == 0) continue;
            CGRect b = [line boundsForPage:page];
            if (b.size.width <= 0 || b.size.height <= 0) continue;
            [lines addObject:@{
                @"page" : @(p),
                @"x" : @(b.origin.x), @"y" : @(b.origin.y),
                @"w" : @(b.size.width), @"h" : @(b.size.height),
                @"text" : text,
            }];
        }
    }
    NSData *data = [NSJSONSerialization dataWithJSONObject:@{@"lines" : lines}
                                                   options:0 error:nil];
    fwrite(data.bytes, 1, data.length, stdout);
    printf("\n");
    return 0;
}

static int applyPlan(NSString *path, NSString *planPath) {
    PDFDocument *doc = [[PDFDocument alloc] initWithURL:[NSURL fileURLWithPath:path]];
    if (!doc) { fprintf(stderr, "could not open PDF\n"); return 1; }
    NSData *planData = [NSData dataWithContentsOfFile:planPath];
    if (!planData) { fprintf(stderr, "could not read plan\n"); return 1; }
    NSError *err = nil;
    NSDictionary *plan = [NSJSONSerialization JSONObjectWithData:planData options:0 error:&err];
    if (!plan) { fprintf(stderr, "bad plan JSON: %s\n", err.localizedDescription.UTF8String); return 1; }

    // idempotence: remove our own annotations from a previous run first
    for (NSUInteger p = 0; p < doc.pageCount; p++) {
        PDFPage *page = [doc pageAtIndex:p];
        for (PDFAnnotation *a in [page.annotations copy]) {
            if ([a.userName isEqualToString:@"anveesa"]) {
                [page removeAnnotation:a];
            }
        }
    }

    NSUInteger count = 0;
    for (NSDictionary *entry in plan[@"lines"]) {
        NSUInteger p = [entry[@"page"] unsignedIntegerValue];
        if (p >= doc.pageCount) continue;
        PDFPage *page = [doc pageAtIndex:p];
        CGRect bounds = CGRectMake([entry[@"x"] doubleValue], [entry[@"y"] doubleValue],
                                   [entry[@"w"] doubleValue], [entry[@"h"] doubleValue]);
        NSString *text = entry[@"text"];

        PDFAnnotation *note =
            [[PDFAnnotation alloc] initWithBounds:bounds
                                          forType:PDFAnnotationSubtypeFreeText
                                   withProperties:nil];
        note.contents = text;
        // fit the text to the line box: shrink until estimated width fits
        CGFloat size = bounds.size.height * 0.62;
        CGFloat estimated = text.length * size * 0.52;
        while (estimated > bounds.size.width && size > 5.0) {
            size *= 0.92;
            estimated = text.length * size * 0.52;
        }
        note.font = [NSFont systemFontOfSize:MAX(size, 5.0)];
        note.fontColor = [NSColor colorWithCalibratedWhite:0.1 alpha:1.0];
        note.color = [NSColor whiteColor]; // opaque background masks the original
        note.userName = @"anveesa";        // tag so a re-run can replace us
        [page addAnnotation:note];
        count++;
    }

    if (![doc writeToURL:[NSURL fileURLWithPath:path]]) {
        fprintf(stderr, "failed to save PDF\n");
        return 1;
    }
    printf("{\"applied\": %lu}\n", (unsigned long)count);
    return 0;
}

int main(int argc, const char *argv[]) {
    @autoreleasepool {
        if (argc >= 3 && strcmp(argv[1], "list") == 0) {
            return listLines([NSString stringWithUTF8String:argv[2]]);
        }
        if (argc >= 4 && strcmp(argv[1], "apply") == 0) {
            return applyPlan([NSString stringWithUTF8String:argv[2]],
                             [NSString stringWithUTF8String:argv[3]]);
        }
        fprintf(stderr, "usage: anveesa-pdfmask list <file.pdf> | apply <file.pdf> <plan.json>\n");
        return 2;
    }
}
